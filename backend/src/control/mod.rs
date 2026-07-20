//! The control layer: business logic expressed as a **plan of semantic operations**.
//!
//! A plan is plain data — a `Vec<Op>` — so it is self-describing: the step count and every
//! step's label are known before anything runs, which is all a linear progress bar needs.
//! Execution is an ordinary procedure that dispatches each op to its closed-loop
//! implementation; the plan never encodes control flow, so there is no interpreter here.
//!
//! ```no_run
//! use mso5202d::control::{execute, Context, Op, SilentProgress};
//! use mso5202d::settings::{Probe, StoreDepth};
//! use mso5202d::Device;
//!
//! let device = Device::connect()?;
//! let plan = vec![
//!     Op::DefaultSetup,
//!     Op::SetChannel { channel: 1, on: true },
//!     Op::SetProbe { channel: 1, probe: Probe::X1 },
//!     Op::SetVoltsPerDiv { channel: 1, millivolts: 1000 },
//!     Op::SetDepth { depth: StoreDepth::K512 },
//! ];
//!
//! let context = Context::new(&device, &SilentProgress);
//! execute(&context, &plan)?;
//! # Ok::<(), mso5202d::Error>(())
//! ```
//!
//! # Failure policy
//!
//! Execution **stops at the first failed op**. Every op here changes how the instrument
//! will acquire, so continuing past a failure would capture data at settings the caller
//! did not ask for — worse than stopping loudly.

pub mod converge;
pub mod csv;
pub mod ops;
pub mod progress;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use tracing::{debug, error, info, info_span};

use crate::device::{Device, Key, Knob};
use crate::error::{Error, Result};
use crate::settings::{Probe, StoreDepth};

pub use csv::CsvSource;
pub use ops::Op;
pub use progress::{ProgressEvent, ProgressSink, SilentProgress, StepState};

/// Number of positions in the probe-attenuation ring (1x, 10x, 100x, 1000x).
const PROBE_RING: u32 = 4;

/// Number of positions in the store-depth ring (4K, 40K, 512K, 1M).
const DEPTH_RING: u32 = 4;

/// Softkey that cycles probe attenuation while a channel menu is open.
const PROBE_SOFTKEY: Key = Key::Fn4;

/// Softkey that cycles store depth while the Acquire menu is open.
const DEPTH_SOFTKEY: Key = Key::Fn5;

/// Menu id shown by the Acquire menu.
const ACQUIRE_MENU: u8 = 17;

/// Menu id shown after a Default Setup.
const DEFAULT_SETUP_MENU: u8 = 25;

/// Trigger level tolerance, in 1/25-division units. The knob does not land on every
/// integer, so demanding an exact value would spin until the step budget ran out.
const TRIGGER_TOLERANCE: i64 = 2;

/// Attempts to toggle a channel into the wanted state.
///
/// Each press is a toggle, so a stale read causes an unwanted flip back — the loop reads
/// between presses and needs enough attempts to ride out a scope that is briefly busy.
/// Measured: a successful toggle routinely takes two iterations.
const CHANNEL_ATTEMPTS: u32 = 5;

/// Settle time after toggling a channel. Longer than a plain knob nudge because enabling a
/// channel reconfigures the acquisition, and reading back too early sees the old state.
const CHANNEL_SETTLE: Duration = Duration::from_millis(700);

/// Menu id of the Save/Recall base menu.
const SAVE_RECALL_MENU: u8 = 47;

/// Menu id of the CSV page and its file list, which share an id.
const CSV_MENU: u8 = 48;

/// Softkey that cycles the CSV Source radio (CH1 → CH2 → LA, wrapping).
const SOURCE_SOFTKEY: Key = Key::Fn1;

/// Softkey that performs the save.
const SAVE_SOFTKEY: Key = Key::Fn2;

/// Softkey that opens the CSV page from the Save/Recall menu.
const CSV_SOFTKEY: Key = Key::Fn3;

/// Softkey that deletes the selected file on the CSV page.
///
/// Shares its key id with the probe softkey — a softkey's meaning depends entirely on the
/// open menu, which is why the CSV page is confirmed before this is ever pressed.
const DELETE_SOFTKEY: Key = Key::Fn4;

/// Gap between delete presses; each one removes a file.
const DELETE_PRESS_GAP: Duration = Duration::from_millis(600);

/// Passes over the card before giving up on clearing it. More than one is needed because
/// the single-slot key mailbox can drop presses.
const CLEAR_ROUNDS: u32 = 4;

/// Attempts to land the Source radio on the wanted entry.
const SOURCE_ATTEMPTS: u32 = 6;

/// Settle time after a Source press before re-reading the screen.
const SOURCE_SETTLE: Duration = Duration::from_secs(1);

/// Gap between the two presses that open the file list and write the file.
const SAVE_PRESS_GAP: Duration = Duration::from_millis(800);

/// How long to wait for the scope to finish writing before concluding a press was dropped.
///
/// A large record takes tens of seconds and the file only appears when the scope renames
/// its temporary file at the very end, so this must be generous: re-pressing during the
/// write corrupts the save and advances the Source radio.
const SAVE_RETRY_GRACE: Duration = Duration::from_secs(45);

/// Longest a save is allowed to take, by record length. A 512 K export is roughly 7.7 MB
/// written to a slow card.
fn save_timeout(depth: StoreDepth) -> Duration {
    Duration::from_secs(match depth {
        StoreDepth::K4 => 30,
        StoreDepth::K40 => 45,
        StoreDepth::K512 => 130,
        StoreDepth::M1 => 220,
        StoreDepth::Unknown(_) => 60,
    })
}

/// Rows an exported CSV holds at each record length — the sample count plus a fixed
/// 64-row margin the scope adds.
fn csv_rows(depth: StoreDepth) -> u64 {
    match depth {
        StoreDepth::K4 => 4_064,
        StoreDepth::K40 => 40_064,
        StoreDepth::K512 => 400_064,
        StoreDepth::M1 => 800_064,
        StoreDepth::Unknown(_) => 4_064,
    }
}

/// Average bytes per `time,volts` row, measured from a real export (78 080 bytes over
/// 4 064 rows). Rows are fixed-width enough that this holds across record lengths.
const CSV_BYTES_PER_ROW: u64 = 19;

/// Expected size of an export, for reporting progress while the card is being written.
///
/// An estimate — unlike the byte count itself, which is measured — but the record length
/// is known in advance and the row format is fixed, so it is close enough to make a long
/// write show meaningful progress instead of sitting still.
fn expected_csv_bytes(depth: StoreDepth) -> u64 {
    csv_rows(depth) * CSV_BYTES_PER_ROW
}

/// Longest to wait for the busy banner to clear.
const BANNER_TIMEOUT: Duration = Duration::from_secs(120);

/// How long to wait for a single sequence to capture before forcing a trigger.
const TRIGGER_FORCE_AFTER: Duration = Duration::from_secs(4);

/// Longest to wait for a single sequence to reach a captured state.
const TRIGGER_TIMEOUT: Duration = Duration::from_secs(25);

/// Where the scope mounts the removable card.
const CARD_PATH: &str = "/mnt/udisk";

/// A file a plan exported and, once downloaded, its contents.
#[derive(Debug, Clone)]
pub struct CapturedFile {
    /// Which trace it holds.
    pub source: CsvSource,
    /// Filename on the card, e.g. `WaveData1410.csv`.
    pub name: String,
    /// Size in bytes as the card reports it.
    pub size: u64,
    /// File contents, once [`Op::Download`] has fetched them.
    pub data: Option<Vec<u8>>,
}

impl CapturedFile {
    /// Absolute path on the scope's filesystem.
    pub fn path(&self) -> String {
        format!("{CARD_PATH}/{}", self.name)
    }
}

/// What a plan produced.
#[derive(Debug, Default)]
pub struct Outputs {
    /// Exported files, in the order they were saved.
    pub files: Vec<CapturedFile>,
}

impl Outputs {
    /// The exported file for `source`, if one was saved.
    pub fn file(&self, source: CsvSource) -> Option<&CapturedFile> {
        self.files.iter().find(|file| file.source == source)
    }
}

/// State carried between ops within one plan.
#[derive(Debug, Default)]
struct Session {
    /// Whether the CSV file list is already open.
    ///
    /// It stays open after any save, which changes how many presses the next save needs —
    /// pressing twice with it open writes a spurious second file.
    filelist_open: bool,
}

/// Execution context: the device, where progress goes, and whether to stop.
///
/// Carrying all three together keeps them out of every op's signature, and gives one place
/// where cancellation is checked — at each step boundary.
pub struct Context<'a> {
    device: &'a Device,
    sink: &'a dyn ProgressSink,
    cancel: Option<&'a AtomicBool>,
    step: RefCell<Step>,
    outputs: RefCell<Outputs>,
    session: RefCell<Session>,
}

/// Where execution currently is, for attributing progress events.
#[derive(Debug, Clone, Default)]
struct Step {
    index: usize,
    total: usize,
    label: String,
}

impl<'a> Context<'a> {
    /// Create a context that cannot be cancelled.
    pub fn new(device: &'a Device, sink: &'a dyn ProgressSink) -> Self {
        Self {
            device,
            sink,
            cancel: None,
            step: RefCell::new(Step::default()),
            outputs: RefCell::new(Outputs::default()),
            session: RefCell::new(Session::default()),
        }
    }

    /// Create a context that stops when `cancel` is set.
    ///
    /// Cancellation is checked at every step boundary, so a plan stops between operations
    /// rather than mid-way through one — the scope is never left half-configured by an
    /// interrupted key sequence.
    pub fn cancellable(
        device: &'a Device,
        sink: &'a dyn ProgressSink,
        cancel: &'a AtomicBool,
    ) -> Self {
        Self {
            device,
            sink,
            cancel: Some(cancel),
            step: RefCell::new(Step::default()),
            outputs: RefCell::new(Outputs::default()),
            session: RefCell::new(Session::default()),
        }
    }

    /// The device this context drives.
    pub fn device(&self) -> &Device {
        self.device
    }

    /// What the plan has produced so far — the exported files and their contents.
    pub fn outputs(&self) -> std::cell::Ref<'_, Outputs> {
        self.outputs.borrow()
    }

    /// Take ownership of the outputs, leaving the context empty.
    pub fn take_outputs(&self) -> Outputs {
        std::mem::take(&mut self.outputs.borrow_mut())
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .map(|flag| flag.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Report sub-progress within the current step.
    ///
    /// Only meaningful where a genuine measure exists (bytes written or transferred);
    /// steps without one simply never call it.
    pub fn advance(&self, done: u64, total: u64) {
        self.emit(StepState::Advanced { done, total });
    }

    fn emit(&self, state: StepState) {
        let step = self.step.borrow();
        self.sink.report(&ProgressEvent {
            index: step.index,
            total: step.total,
            label: step.label.clone(),
            state,
        });
    }
}

/// Run a plan, reporting progress and stopping at the first failure.
///
/// The plan's shape is known up front, so the first event already carries the final step
/// count and total weight.
pub fn execute(context: &Context, plan: &[Op]) -> Result<()> {
    let span = info_span!("plan", steps = plan.len());
    let _entered = span.enter();
    info!("starting plan of {} operations", plan.len());

    for (index, op) in plan.iter().enumerate() {
        if context.is_cancelled() {
            info!(index, "plan cancelled before step");
            return Err(Error::Cancelled);
        }

        let label = op.label();
        *context.step.borrow_mut() = Step {
            index,
            total: plan.len(),
            label: label.clone(),
        };

        let step_span = info_span!("step", index, %label);
        let _step_entered = step_span.enter();

        // Logged before the work starts: a hang shows up as a step that began and never
        // completed, which is precisely the evidence needed to find it.
        info!("begin");
        context.emit(StepState::Started);

        let started = Instant::now();
        match run(context, op) {
            Ok(()) => {
                let elapsed_ms = started.elapsed().as_millis() as u64;
                info!(elapsed_ms, "done");
                context.emit(StepState::Completed { elapsed_ms });
            }
            Err(e) => {
                error!(error = %e, "failed");
                context.emit(StepState::Failed {
                    error: e.to_string(),
                });
                return Err(e);
            }
        }
    }

    info!("plan complete");
    Ok(())
}

/// Dispatch one op to its implementation.
fn run(context: &Context, op: &Op) -> Result<()> {
    let device = context.device;
    match *op {
        Op::DefaultSetup => default_setup(device),
        Op::SetChannel { channel, on } => set_channel(device, channel, on),
        Op::SetProbe { channel, probe } => set_probe(device, channel, probe),
        Op::SetVoltsPerDiv { channel, millivolts } => {
            set_volts_per_div(device, channel, millivolts)
        }
        Op::SetTimePerDiv { nanoseconds } => set_time_per_div(device, nanoseconds),
        Op::SetTriggerLevel { position } => set_trigger_level(device, position),
        Op::SetDepth { depth } => set_depth(device, depth),
        Op::CaptureSingle => capture_single(device),
        Op::SaveCsv { source } => save_csv(context, source),
        Op::Download { source } => download(context, source),
        Op::ClearCard => clear_card(context),
    }
}

// --- operation implementations ----------------------------------------------

/// Factory Default Setup, confirmed by the menu the scope lands on.
fn default_setup(device: &Device) -> Result<()> {
    converge::open_menu(device, Key::DefaultSetup, &[DEFAULT_SETUP_MENU])?;
    debug!("default setup confirmed");
    Ok(())
}

/// Show or hide a channel.
///
/// The channel button is a **toggle**, so this reads the current state and presses only
/// when a flip is actually needed — pressing blindly would be a coin flip.
fn set_channel(device: &Device, channel: u8, on: bool) -> Result<()> {
    let key = channel_key(channel)?;
    for _ in 0..CHANNEL_ATTEMPTS {
        if device.read_settings()?.channel_shown(channel) == on {
            return Ok(());
        }
        device.press(key)?;
        sleep(CHANNEL_SETTLE);
    }
    if device.read_settings()?.channel_shown(channel) == on {
        return Ok(());
    }
    Err(Error::Unexpected(format!(
        "CH{channel} would not turn {}",
        if on { "on" } else { "off" }
    )))
}

/// Set probe attenuation by cycling the channel menu's probe softkey.
///
/// The softkey only means "probe" while that channel's menu is open — softkey meaning is
/// menu-dependent — so the menu is opened and confirmed first.
fn set_probe(device: &Device, channel: u8, probe: Probe) -> Result<()> {
    let target = probe
        .factor()
        .ok_or_else(|| Error::Unexpected(format!("probe {probe:?} has no known code")))?;

    converge::open_menu(device, channel_key(channel)?, &[channel_menu(channel)?])?;
    let field = format!("VERT-CH{channel}-PROBE");
    let code = probe_code(probe)?;
    converge::cycle_until(
        device,
        PROBE_SOFTKEY,
        PROBE_RING,
        |settings| settings.field(&field).map(|value| value as i64),
        code,
    )?;
    debug!(channel, target, "probe set");
    Ok(())
}

/// Set a channel's vertical scale by stepping its volts/div knob.
///
/// The channel must be on: its volts/div key is inert while the channel is hidden.
fn set_volts_per_div(device: &Device, channel: u8, millivolts: u32) -> Result<()> {
    if !device.read_settings()?.channel_shown(channel) {
        return Err(Error::Unexpected(format!(
            "CH{channel} is off, so its volts/div knob is inert — turn the channel on first"
        )));
    }
    let knob = Knob::volts_per_div(channel)
        .ok_or_else(|| Error::Unexpected(format!("no volts/div knob for CH{channel}")))?;
    converge::converge(device, knob, millivolts as i64, 0, |settings| {
        settings.volts_per_div_mv(channel).map(|mv| mv as i64)
    })?;
    Ok(())
}

/// Set the horizontal timebase by stepping the SEC/DIV knob.
fn set_time_per_div(device: &Device, nanoseconds: u64) -> Result<()> {
    converge::converge(device, Knob::TimePerDiv, nanoseconds as i64, 0, |settings| {
        settings.time_per_div_ns().map(|ns| ns as i64)
    })?;
    Ok(())
}

/// Set the trigger level by stepping the trigger knob.
fn set_trigger_level(device: &Device, position: i64) -> Result<()> {
    converge::converge(
        device,
        Knob::TriggerLevel,
        position,
        TRIGGER_TOLERANCE,
        |settings| Some(settings.trigger_position()),
    )?;
    Ok(())
}

/// Set the acquisition record length via the Acquire menu's depth softkey.
///
/// Driven by the softkey rather than a settings write so the on-screen LongMem indicator
/// stays truthful — a raw write changes the acquisition but leaves the display stale.
fn set_depth(device: &Device, depth: StoreDepth) -> Result<()> {
    let code = depth
        .code()
        .ok_or_else(|| Error::Unexpected(format!("depth {depth:?} has no known code")))?;

    converge::open_menu(device, Key::MenuAcquire, &[ACQUIRE_MENU])?;
    converge::cycle_until(
        device,
        DEPTH_SOFTKEY,
        DEPTH_RING,
        |settings| settings.field("ACQURIE-STORE-DEPTH").map(|value| value as i64),
        code as i64,
    )?;
    debug!(?depth, "store depth set");
    Ok(())
}

// --- capture and export -----------------------------------------------------

/// Arm a single sequence and wait for the scope to capture and stop itself.
///
/// The scope must be **running** first: a single sequence armed from a stopped scope
/// latches the stale buffer instead of acquiring afresh. If no edge arrives within a few
/// seconds a trigger is forced once, so a quiet signal still yields a record.
///
/// Afterwards the scope is left **stopped** — that is the captured state, and pressing
/// run/stop here would start it running again and discard the record.
fn capture_single(device: &Device) -> Result<()> {
    if device.read_settings()?.trig_state().is_stopped() {
        debug!("scope was stopped — starting it before arming");
        device.press(Key::RunStop)?;
        sleep(converge::SETTLE);
    }

    device.press(Key::Single)?;
    sleep(Duration::from_millis(500));

    let started = Instant::now();
    let mut forced = false;
    while started.elapsed() < TRIGGER_TIMEOUT {
        let state = device.read_settings()?.trig_state();
        if state.is_stopped() {
            debug!(?state, "single sequence captured");
            return Ok(());
        }
        if !forced && started.elapsed() > TRIGGER_FORCE_AFTER {
            debug!("no trigger yet — forcing one");
            device.press(Key::ForceTrigger)?;
            forced = true;
        }
        sleep(Duration::from_millis(400));
    }
    Err(Error::Unexpected(
        "single sequence never reached a captured state — nothing to save".into(),
    ))
}

/// Export the captured record for `source` to the memory card.
fn save_csv(context: &Context, source: CsvSource) -> Result<()> {
    let device = context.device;

    // Saving without a captured record writes no file at all, so refuse early with a clear
    // reason rather than waiting out the full timeout for a file that will never appear.
    let settings = device.read_settings()?;
    if !settings.trig_state().is_stopped() {
        return Err(Error::Unexpected(format!(
            "scope is {:?}, not holding a captured record — capture before saving",
            settings.trig_state()
        )));
    }
    let depth = settings.store_depth();

    open_csv_menu(device)?;
    select_source(device, source)?;

    let before: Vec<String> = list_wavedata(device)?
        .into_iter()
        .map(|file| file.name)
        .collect();

    // The file list stays open after any save, and that changes the press count: two
    // presses when closed (open, then write), one when already open. Pressing twice with it
    // open writes a second, spurious file.
    let filelist_open = context.session.borrow().filelist_open;
    if !filelist_open {
        device.press(SAVE_SOFTKEY)?;
        sleep(SAVE_PRESS_GAP);
    }
    device.press(SAVE_SOFTKEY)?;
    context.session.borrow_mut().filelist_open = true;

    let file = await_new_file(context, &before, save_timeout(depth), expected_csv_bytes(depth))?;
    info!(name = %file.name, size = file.size, "saved");

    context.outputs.borrow_mut().files.push(CapturedFile {
        source,
        name: file.name,
        size: file.size,
        data: None,
    });

    // The write is asynchronous: the scope ignores keys until the banner clears, so let it
    // finish before the next operation presses anything.
    await_save_finished(device)?;
    Ok(())
}

/// Read back the CSV saved for `source`.
fn download(context: &Context, source: CsvSource) -> Result<()> {
    let device = context.device;
    let (path, expected) = {
        let outputs = context.outputs.borrow();
        let file = outputs.file(source).ok_or_else(|| {
            Error::Unexpected(format!(
                "no saved file for {} — a SaveCsv step must run first",
                source.name()
            ))
        })?;
        (file.path(), file.size)
    };

    let data = device.download_with(&path, |done| context.advance(done, expected))?;

    // The transfer declares no length, so a truncated read looks like a short file. The
    // card's own size is the independent check that catches it.
    if data.len() as u64 != expected {
        return Err(Error::Unexpected(format!(
            "{path}: downloaded {} bytes but the card reports {expected}",
            data.len()
        )));
    }
    info!(%path, bytes = data.len(), "downloaded");

    let mut outputs = context.outputs.borrow_mut();
    if let Some(file) = outputs.files.iter_mut().find(|f| f.source == source) {
        file.data = Some(data);
    }
    Ok(())
}

/// Delete every exported waveform CSV from the card.
///
/// Uses the front-panel delete key rather than a shell `rm`. There is **no confirmation
/// dialog**: the first press opens the file list and every press after that deletes the
/// selected file, so the press count has to be exact — one press too many would delete a
/// file that was never counted.
fn clear_card(context: &Context) -> Result<()> {
    let device = context.device;
    open_csv_menu(device)?;

    let initial = list_wavedata(device)?.len();
    if initial == 0 {
        info!("card already holds no exported CSVs");
        return Ok(());
    }
    info!(files = initial, "clearing exported CSVs from the card");

    // Whether the file list is already open decides the press count: opening it costs one
    // press, and only the presses after that delete anything.
    let mut list_open = context.session.borrow().filelist_open;

    for round in 0..CLEAR_ROUNDS {
        let remaining = list_wavedata(device)?.len();
        if remaining == 0 {
            break;
        }
        context.advance((initial - remaining) as u64, initial as u64);

        let presses = if list_open { remaining } else { remaining + 1 };
        debug!(round, remaining, presses, "deleting");
        for _ in 0..presses {
            device.press(DELETE_SOFTKEY)?;
            sleep(DELETE_PRESS_GAP);
        }
        list_open = true;
    }

    // The delete key leaves the file list open. Recording that matters for a later save:
    // assuming it is open costs at most one dropped press, which the save's retry recovers,
    // whereas wrongly assuming it is closed makes the save press twice and write a
    // spurious second file.
    context.session.borrow_mut().filelist_open = true;

    let left = list_wavedata(device)?.len();
    context.advance((initial - left) as u64, initial as u64);
    if left > 0 {
        return Err(Error::Unexpected(format!(
            "{left} exported CSV(s) still on the card after {CLEAR_ROUNDS} passes"
        )));
    }
    info!(deleted = initial, "card cleared");
    Ok(())
}

// --- save-flow helpers ------------------------------------------------------

/// Ensure the CSV page is on screen, opening it if needed.
///
/// Idempotent: if the CSV page (or its file list, which shares the menu id) is already
/// showing, this does nothing — pressing the menu key again would navigate away.
fn open_csv_menu(device: &Device) -> Result<()> {
    if device.read_settings()?.menu_id() == CSV_MENU {
        return Ok(());
    }
    converge::open_menu(device, Key::MenuSaveRecall, &[SAVE_RECALL_MENU, CSV_MENU])?;
    if device.read_settings()?.menu_id() != CSV_MENU {
        converge::open_menu(device, CSV_SOFTKEY, &[CSV_MENU])?;
    }
    Ok(())
}

/// Cycle the Source radio until it reads `source`.
///
/// The selection is not in the settings block, so each press is verified against the
/// rendered screen. Getting this wrong silently exports the wrong channel.
fn select_source(device: &Device, source: CsvSource) -> Result<()> {
    for _ in 0..SOURCE_ATTEMPTS {
        if csv::selected_source(&device.screenshot()?) == Some(source) {
            device.transport().resync();
            sleep(Duration::from_millis(400));
            return Ok(());
        }
        device.press(SOURCE_SOFTKEY)?;
        sleep(SOURCE_SETTLE);
    }
    Err(Error::Unexpected(format!(
        "could not select CSV source {} — the radio never showed it as selected",
        source.name()
    )))
}

/// Wait for a WaveData file that was not in `before` to appear and stop growing.
///
/// Two waits, both necessary: the file only becomes visible when the scope renames its
/// temporary file, and on a slow card it is visible while still being written, so a stable
/// size is the completion signal.
fn await_new_file(
    context: &Context,
    before: &[String],
    timeout: Duration,
    expected: u64,
) -> Result<FoundFile> {
    let device = context.device;
    let started = Instant::now();
    let mut last_press = Instant::now();
    let mut target: Option<String> = None;

    while started.elapsed() < timeout {
        if context.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let listing = list_wavedata_if_reachable(device);
        if let Some(found) = listing
            .iter()
            .find(|file| !before.contains(&file.name))
        {
            target = Some(found.name.clone());
            break;
        }
        // Only after a long grace: re-pressing during the write corrupts the save and
        // advances the Source radio. Nothing appearing this late means a dropped press.
        if last_press.elapsed() > SAVE_RETRY_GRACE {
            debug!("no file after the grace period — re-pressing save");
            device.press(SAVE_SOFTKEY)?;
            last_press = Instant::now();
        }
        sleep(Duration::from_millis(800));
    }

    let name = target.ok_or_else(|| {
        Error::Unexpected(format!(
            "no CSV appeared on the card within {timeout:?} — is a card inserted and mounted?"
        ))
    })?;

    // Now wait for the size to settle.
    let mut last_size = u64::MAX;
    let mut stable = 0;
    while started.elapsed() < timeout {
        let size = list_wavedata_if_reachable(device)
            .into_iter()
            .find(|file| file.name == name)
            .map(|file| file.size)
            .unwrap_or(0);
        if size > 0 {
            // Report against the expected size so a long write actually moves; the byte
            // count is real even though the total is an estimate.
            context.advance(size.min(expected), expected);
            if size == last_size {
                stable += 1;
                if stable >= 2 {
                    return Ok(FoundFile { name, size });
                }
            } else {
                stable = 0;
            }
            last_size = size;
        }
        sleep(Duration::from_millis(700));
    }
    Err(Error::Unexpected(format!(
        "{name} never stopped growing within {timeout:?}"
    )))
}

/// Wait for the "operation in progress" banner to clear.
///
/// While it is up the scope ignores key presses, so anything pressed during this window is
/// silently dropped — which is how a save ends up writing the same channel twice.
fn await_save_finished(device: &Device) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < BANNER_TIMEOUT {
        match device.screenshot() {
            Ok(screen) if !csv::save_in_progress(&screen) => {
                sleep(Duration::from_millis(500));
                return Ok(());
            }
            Ok(_) => debug!("save still finishing"),
            Err(e) => debug!(error = %e, "framebuffer grab failed while waiting"),
        }
        sleep(Duration::from_millis(1500));
    }
    Err(Error::Unexpected(
        "the scope stayed busy after the save; it may still be writing".into(),
    ))
}

/// A file found on the card.
struct FoundFile {
    name: String,
    size: u64,
}

/// The exported waveform files currently on the card.
fn list_wavedata(device: &Device) -> Result<Vec<crate::device::FileEntry>> {
    Ok(csv::wavedata_files(&device.list_dir(CARD_PATH)?))
}

/// List the card, treating a failure as "cannot tell yet" rather than an error.
///
/// While the scope writes a large record it stops answering the shell channel entirely —
/// a 512 K export takes it offline for tens of seconds. A timeout there means "still
/// busy", not "no files", so polling must ride it out instead of aborting the save.
fn list_wavedata_if_reachable(device: &Device) -> Vec<crate::device::FileEntry> {
    match list_wavedata(device) {
        Ok(files) => files,
        Err(e) => {
            debug!(error = %e, "card listing unavailable — scope busy writing");
            Vec::new()
        }
    }
}

// --- small lookups ----------------------------------------------------------

fn channel_key(channel: u8) -> Result<Key> {
    match channel {
        1 => Ok(Key::Ch1Menu),
        2 => Ok(Key::Ch2Menu),
        other => Err(Error::Unexpected(format!("no such channel: CH{other}"))),
    }
}

fn channel_menu(channel: u8) -> Result<u8> {
    match channel {
        1 => Ok(1),
        2 => Ok(2),
        other => Err(Error::Unexpected(format!("no such channel: CH{other}"))),
    }
}

fn probe_code(probe: Probe) -> Result<i64> {
    Ok(match probe {
        Probe::X1 => 0,
        Probe::X10 => 1,
        Probe::X100 => 2,
        Probe::X1000 => 3,
        Probe::Unknown(code) => code as i64,
    })
}
