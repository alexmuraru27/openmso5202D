//! `mso5202d` — reverse-engineered driver for the Hantek MSO5202D oscilloscope
//! (USB `049f:505a`).
//!
//! The crate is layered so each level has one job and depends only on the one below:
//!
//! | Layer | Module | Concern |
//! |---|---|---|
//! | 3 | [`control`] | **Business logic** — plans of semantic operations, closed-loop |
//! | 2 | [`device`] | **Device operations** — keys, knobs, settings, screen, files, shell |
//! | 1 | [`usb`] | USB transport — connect, bind, transact |
//! | 0 | [`protocol`], [`settings`], [`waveform`], [`decoder`] | Wire format, data layout, decoding (pure logic) |
//!
//! [`Device`] is the entry point:
//!
//! ```no_run
//! use mso5202d::{Device, Key};
//!
//! let scope = Device::connect()?;
//! let settings = scope.read_settings()?;
//! println!("timebase: {:?} ns/div", settings.time_per_div_ns());
//! scope.press(Key::Autoset)?;
//! # Ok::<(), mso5202d::Error>(())
//! ```
//!
//! Hardware access needs the scope plugged in, and either the udev rule from
//! `70-mso5202d.rules` installed or the process running as root.
//!
//! # Configuration policy
//!
//! The settings block is treated as **read-only**. The scope is configured exclusively
//! through key events, because a raw block write skips the firmware side effects a real
//! key press runs — LEDs, on-screen state, acquisition reconfiguration, and SD-card
//! detection. No write path is exposed here by design.

pub mod control;
pub mod decoder;
pub mod device;
pub mod error;
pub mod logging;
pub mod protocol;
pub mod settings;
pub mod usb;
pub mod waveform;

pub use control::{execute, Context, Op, ProgressEvent, ProgressSink, StepState};
pub use device::{Device, FileEntry, Key, Knob, Screenshot, Turn};
pub use error::{Error, Result};
pub use protocol::{PID, VID};
pub use settings::{Settings, StoreDepth, TrigState};
pub use usb::Transport;
