//! Progress reporting for a running plan.
//!
//! The backend emits **events**; drawing a bar from them is the UI's job. What the backend
//! guarantees is enough structure to make that possible without guesswork:
//!
//! - the **total** number of steps is known before execution starts, so a bar can be
//!   linear over `index / total` from the first event,
//! - long steps may report **sub-progress** where a real measure exists (bytes written to
//!   the card, bytes transferred), so a step that dominates a run still moves.
//!
//! There is deliberately no per-step weighting. Duration turned out not to be a property
//! of the operation but of how far the instrument's current state is from the target —
//! the same op measured 4399 ms and 908 ms on consecutive runs — so any static estimate
//! would be wrong. Where a step is genuinely long, [`StepState::Advanced`] reports its
//! real progress, which is both simpler and more accurate than a guessed weight.
//!
//! Retries and read-back verification inside a step are invisible here: one semantic
//! operation is one step, however many key presses it took to land.

use std::fmt;

/// What happened to a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepState {
    /// The step has begun.
    Started,
    /// Sub-progress within the step, where a genuine measure exists. Steps without one
    /// simply never emit this.
    Advanced {
        /// Units completed so far.
        done: u64,
        /// Units expected in total.
        total: u64,
    },
    /// The step finished successfully.
    Completed {
        /// Wall-clock duration of the step.
        elapsed_ms: u64,
    },
    /// The step failed; the plan stops here.
    Failed {
        /// Human-readable cause.
        error: String,
    },
}

/// One progress notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressEvent {
    /// Zero-based index of the step within the plan.
    pub index: usize,
    /// Total number of steps in the plan, known before execution starts.
    pub total: usize,
    /// Human-readable description, e.g. `"Turning on CH1"`.
    pub label: String,
    /// What happened.
    pub state: StepState,
}

impl fmt::Display for ProgressEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let step = format!("[{}/{}]", self.index + 1, self.total);
        match &self.state {
            StepState::Started => write!(f, "{step} {}", self.label),
            StepState::Advanced { done, total } => {
                write!(f, "{step} {} — {done}/{total}", self.label)
            }
            StepState::Completed { elapsed_ms } => {
                write!(f, "{step} {} — done in {elapsed_ms} ms", self.label)
            }
            StepState::Failed { error } => write!(f, "{step} {} — FAILED: {error}", self.label),
        }
    }
}

/// Receives progress events during plan execution.
///
/// Implemented for any `Fn(&ProgressEvent)`, so a closure is usually enough:
///
/// ```
/// use mso5202d::control::ProgressEvent;
/// let sink = |event: &ProgressEvent| println!("{event}");
/// # let _ = &sink;
/// ```
pub trait ProgressSink {
    /// Handle one event. Must not block for long — it runs inline with device I/O.
    fn report(&self, event: &ProgressEvent);
}

impl<F: Fn(&ProgressEvent)> ProgressSink for F {
    fn report(&self, event: &ProgressEvent) {
        self(event)
    }
}

/// A sink that discards everything, for callers that do not care about progress.
pub struct SilentProgress;

impl ProgressSink for SilentProgress {
    fn report(&self, _event: &ProgressEvent) {}
}
