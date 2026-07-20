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
pub mod ops;
pub mod progress;

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use tracing::{debug, error, info, info_span};

use crate::device::{Device, Key, Knob};
use crate::error::{Error, Result};
use crate::settings::{Probe, StoreDepth};

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

/// Execution context: the device, where progress goes, and whether to stop.
///
/// Carrying all three together keeps them out of every op's signature, and gives one place
/// where cancellation is checked — at each step boundary.
pub struct Context<'a> {
    device: &'a Device,
    sink: &'a dyn ProgressSink,
    cancel: Option<&'a AtomicBool>,
    step: RefCell<Step>,
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
        }
    }

    /// The device this context drives.
    pub fn device(&self) -> &Device {
        self.device
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
    for _ in 0..3 {
        if device.read_settings()?.channel_shown(channel) == on {
            return Ok(());
        }
        device.press(key)?;
        std::thread::sleep(converge::SETTLE);
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
