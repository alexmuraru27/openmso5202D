//! The capture workflows: **prepare** then **capture**, as cohesive backend operations.
//!
//! This is the faithful port of `prepare_capture` / `capture_prepared` / `deep_capture`
//! from `mso5202d_plot.py`, expressed on top of the [`Op`] plan model so each workflow
//! still streams progress step by step. Splitting prepare from capture mirrors the Python
//! plotter's two buttons: prepare is the slow, idempotent setup (run once); capture is the
//! fast, **re-pressable** part that arms a single sequence and reads the record back.
//!
//! The logic pod is intentionally absent — this project never uses LA — so a capture is
//! always one or two analog channels, and the whole flow is **key-only** (the settings
//! block is read but never written; a raw block write skips the firmware side effects a
//! real key runs, including SD-card detection, and breaks the save path).
//!
//! ```no_run
//! use mso5202d::control::{capture, Context, SilentProgress};
//! use mso5202d::settings::StoreDepth;
//! use mso5202d::Device;
//!
//! let device = Device::connect_without_reset()?;
//! let spec = capture::CaptureSpec {
//!     channels: vec![1, 2],
//!     depth: StoreDepth::K40,
//!     timebase_ns: Some(2_000),
//!     ..Default::default()
//! };
//! let context = Context::new(&device, &SilentProgress);
//! capture::prepare(&context, &spec)?;   // once — reset, channels, scale, depth, timebase
//! capture::capture(&context, &spec)?;   // re-pressable — arm, wait, export, read back
//! for file in &context.outputs().files {
//!     println!("{} = {} bytes", file.source.name(), file.size);
//! }
//! # Ok::<(), mso5202d::Error>(())
//! ```

use crate::control::{execute, Context, CsvSource, Op};
use crate::error::Result;
use crate::settings::{Probe, StoreDepth};

/// What a capture asks of the scope — the analog subset of the Python `deep_capture`
/// parameters (LA dropped).
#[derive(Debug, Clone)]
pub struct CaptureSpec {
    /// Analog channels to acquire: `[1]`, `[2]`, or `[1, 2]`.
    pub channels: Vec<u8>,
    /// Acquisition record length.
    pub depth: StoreDepth,
    /// Vertical scale for every captured channel, in millivolts per division.
    pub volts_per_div_mv: u32,
    /// Explicit SEC/DIV in nanoseconds, or `None` to leave the current timebase.
    ///
    /// Compute it from the fastest signal you want to resolve: the deep record spans 20
    /// divisions at `200 × depth_multiplier` samples/div, so `time/div = period × samples/div
    /// ÷ target_samples_per_bit`.
    pub timebase_ns: Option<u64>,
    /// Trigger level in 1/25-division units above screen centre.
    pub trigger_position: i64,
    /// What the trigger should look for.
    ///
    /// Applied during **prepare**, after the reset and the channel setup — the reset would
    /// undo it, and Alter mirrors settings per channel, so both have to be settled first.
    /// `None` keeps whatever the scope is already triggering on and only sets the level.
    pub trigger: Option<crate::control::trigger::TriggerSetup>,
    /// Targets for the trigger's knob-only values, applied after the trigger itself.
    ///
    /// Kept here rather than in [`TriggerSetup`] because they are not part of what the
    /// trigger *is*: the scope has no way of being told them, so they are walked to with the
    /// knob, and only a plan has the standing to spend that time.
    pub trigger_values: Vec<(crate::control::trigger::Adjustable, i64)>,
    /// Do a factory Default Setup first, for an idempotent known start state.
    pub reset: bool,
    /// How long to wait for the single sequence to trigger, in seconds.
    pub wait_trig_s: u64,
    /// Delete the exported CSVs off the card once they have been read back.
    pub delete_after: bool,
}

impl Default for CaptureSpec {
    fn default() -> Self {
        Self {
            channels: vec![1],
            depth: StoreDepth::K4,
            // A 3.3 V logic signal sits ~3.3 divisions tall at 1 V/div — on screen, no clip.
            volts_per_div_mv: 1000,
            timebase_ns: None,
            // ≈0.5 V at 1 V/div — ABOVE the idle baseline so a framed/bursty line triggers on
            // a real burst, not on idle-noise 0 V crossings, yet well below the 3.3 V peak so
            // every burst crosses it. The value `_set_trig_level_via_keys` targets by default.
            trigger_position: 13,
            trigger: None,
            trigger_values: Vec::new(),
            reset: true,
            wait_trig_s: 30,
            delete_after: false,
        }
    }
}

impl CaptureSpec {
    /// The **prepare** plan: known state, then the wanted channels and their scale, then the
    /// timing. Every step is a closed-loop, key-only op.
    ///
    /// Default Setup leaves CH1 on and CH2 off at 100 mV/div with a 10× probe, so each
    /// channel is driven to its wanted display state, then (for the ones that are on) to 1×
    /// probe and the requested volts/div — both off their Default-Setup values, and both
    /// necessary: at 10× a direct-wired 3.3 V signal reads 33 V and clips off-screen.
    pub fn prepare_plan(&self) -> Vec<Op> {
        let mut plan = Vec::new();
        if self.reset {
            plan.push(Op::DefaultSetup);
        }
        for channel in [1u8, 2] {
            let on = self.channels.contains(&channel);
            plan.push(Op::SetChannel { channel, on });
            if on {
                plan.push(Op::SetProbe {
                    channel,
                    probe: Probe::X1,
                });
                plan.push(Op::SetVoltsPerDiv {
                    channel,
                    millivolts: self.volts_per_div_mv,
                });
            }
        }
        if let Some(nanoseconds) = self.timebase_ns {
            plan.push(Op::SetTimePerDiv { nanoseconds });
        }
        // The trigger goes after the channels: a Default Setup would undo it, and Alter
        // configures each channel on its own page, which needs the channels settled.
        if let Some(setup) = self.trigger {
            plan.push(Op::SetTrigger { setup });
        }
        for &(what, target) in &self.trigger_values {
            plan.push(Op::SetTriggerValue { what, target });
        }
        // The level is meaningless in the modes whose knob is inert, and converging on it
        // there fails with a phantom end stop.
        if self.trigger.is_none_or(|setup| setup.kind.has_level()) {
            plan.push(Op::SetTriggerLevel {
                position: self.trigger_position,
            });
        }
        plan.push(Op::SetDepth { depth: self.depth });
        plan
    }

    /// The **capture** plan: arm a single sequence, wait for it to fire, export each channel
    /// to the card, then read each back.
    ///
    /// Saves are done for **all** channels first, then the read-backs — the same deferral the
    /// Python `capture_prepared` uses, so a multi-megabyte read never sits between Source
    /// changes. The order is deterministic (CH1 before CH2); each `SaveCsv` selects its
    /// Source explicitly (framebuffer-verified) and waits out its own asynchronous write, so
    /// there is no blind cycling or label-guessing.
    pub fn capture_plan(&self) -> Vec<Op> {
        let mut plan = vec![
            Op::ArmSingle,
            Op::WaitCaptured {
                timeout_s: self.wait_trig_s,
            },
        ];
        for source in self.sources() {
            plan.push(Op::SaveCsv { source });
        }
        for source in self.sources() {
            plan.push(Op::Download { source });
        }
        if self.delete_after {
            plan.push(Op::ClearCard);
        }
        plan
    }

    /// The CSV sources to export, in deterministic CH1-before-CH2 order, whatever order the
    /// caller listed the channels in.
    fn sources(&self) -> Vec<CsvSource> {
        let mut sources: Vec<CsvSource> = self
            .channels
            .iter()
            .filter_map(|&ch| source_of(ch))
            .collect();
        sources.sort_by_key(|s| s.name()); // "CH1" < "CH2"
        sources.dedup();
        sources
    }
}

/// Run the **prepare** half — the idempotent setup. Leaves the scope configured and running,
/// ready for [`capture`]. Streams one progress event per op.
pub fn prepare(context: &Context, spec: &CaptureSpec) -> Result<()> {
    // Start from a clean frame boundary — a previous run can leave frames queued, and they
    // would be read as this operation's replies.
    context.device().clear_link();
    execute(context, &spec.prepare_plan())
}

/// Run the **capture** half — arm, wait for the trigger, export and read back each channel.
///
/// The exported files (and their downloaded contents) land in [`Context::outputs`]. Whatever
/// the outcome, the scope is left live and clean: RUN resumed from the single-sequence stop
/// and the file list closed. Re-pressable — no reset, no re-configure.
pub fn capture(context: &Context, spec: &CaptureSpec) -> Result<()> {
    context.device().clear_link();
    let result = execute(context, &spec.capture_plan());
    // Best-effort tidy-up, run whether the plan succeeded or failed, so the scope is never
    // left stopped on the file list.
    super::finalize_capture(context.device());
    result
}

/// **prepare** then **capture** in one call — the one-shot `deep_capture` equivalent.
pub fn deep_capture(context: &Context, spec: &CaptureSpec) -> Result<()> {
    prepare(context, spec)?;
    capture(context, spec)
}

/// Map an analog channel number to its CSV source.
fn source_of(channel: u8) -> Option<CsvSource> {
    match channel {
        1 => Some(CsvSource::Ch1),
        2 => Some(CsvSource::Ch2),
        _ => None,
    }
}

/// The ideal SEC/DIV, in nanoseconds, for a known highest bit/clock period so the deep
/// record puts about `target_samples` samples on each bit — a faithful port of
/// `deep_tdiv_for_bit`.
///
/// The record is acquired over exactly **20 divisions** with `4000 × mult + 64` rows at each
/// depth, so deep samples-per-div = `(rows − 64) / 20`. Setting `deep_dt = bit_period /
/// target_samples` gives `time/div = bit_period × samples_per_div / target_samples`. The
/// caller supplies the bit period as `bit_ns = 1e9 / max_freq_hz`, and should snap the result
/// to the nearest rung the scope offers before driving [`Op::SetTimePerDiv`].
///
/// (The `0x02` on-screen buffer is a different, fixed 200-samples/div view of 19.2 of those
/// 20 divisions; this is the DEEP-record geometry.)
pub fn deep_tdiv_for_bit(bit_ns: f64, depth: StoreDepth, target_samples: f64) -> f64 {
    let rows = super::csv_rows(depth);
    let samples_per_div = (rows.saturating_sub(64)) as f64 / 20.0;
    bit_ns * samples_per_div / target_samples
}
