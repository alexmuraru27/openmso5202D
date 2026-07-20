//! The device-operations layer: named, typed building blocks for driving the scope.
//!
//! [`Device`] sits on [`Transport`] and turns wire selectors into instrument actions —
//! press a key, turn a knob, read the settings, grab the screen, download a file, list the
//! card. Each method is **one logical operation**: it performs its exchange and returns.
//!
//! Deliberately *not* here, because they are policy rather than mechanism:
//!
//! - closed-loop targeting ("set volts/div to 1 V" = press, read back, repeat),
//! - menu navigation and verification,
//! - multi-step workflows such as deep capture or save-to-CSV,
//! - waveform, CSV or serial-protocol decoding.
//!
//! Those belong in the layer above, which composes these blocks.

pub mod files;
pub mod keys;
pub mod screen;
pub mod shell;

use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::protocol::{self, selector, subtype};
use crate::settings::Settings;
use crate::usb::Transport;

pub use files::FileEntry;
pub use keys::{Key, Knob, Turn};
pub use screen::Screenshot;

/// Delay between repeated key events.
///
/// The scope receives keys through a **single-slot mailbox**: two frames sent back to back
/// can collapse into one press. This spacing makes repeats land. It does not *guarantee*
/// them — callers that need certainty should verify against the settings block.
pub const KEY_REPEAT_DELAY: Duration = Duration::from_millis(60);

/// Timeout for a screen grab, which moves ~768 KB.
const SCREENSHOT_TIMEOUT: Duration = Duration::from_millis(4000);

/// Timeout for a file download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_millis(4000);

/// Timeout for the first frame of a shell reply.
const SHELL_TIMEOUT: Duration = Duration::from_millis(4000);

/// Timeout for shell continuation frames — short, because absence means "no more output".
const SHELL_TAIL_TIMEOUT: Duration = Duration::from_millis(300);

/// How many times to re-issue a shell command whose reply arrived without our marker.
const SHELL_ATTEMPTS: u32 = 5;

/// How many times to re-read a settings block that came back malformed.
const SETTINGS_ATTEMPTS: u32 = 5;

/// The oscilloscope, exposed as a set of operations.
pub struct Device {
    transport: Transport,
    /// Source of unique shell reply markers.
    shell_sequence: AtomicU64,
}

impl Device {
    /// Connect to the scope, resetting the USB link first.
    ///
    /// Prefer [`Device::connect_without_reset`] for anything that touches the SD card: a
    /// USB reset disturbs the scope's own USB host controller, which is what the card is
    /// attached to, and saves then fail with "USB device undetected".
    pub fn connect() -> Result<Self> {
        Self::with_transport(Transport::open(true)?)
    }

    /// Connect **without** resetting the USB link. Required for card/save workflows.
    pub fn connect_without_reset() -> Result<Self> {
        Self::with_transport(Transport::open(false)?)
    }

    /// Wrap an already-open transport.
    pub fn with_transport(transport: Transport) -> Result<Self> {
        Ok(Self {
            transport,
            shell_sequence: AtomicU64::new(0),
        })
    }

    /// Borrow the underlying transport, for operations this layer does not cover yet.
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Re-establish the USB connection after an unrecoverable error.
    pub fn reconnect(&mut self, reset: bool) -> Result<()> {
        self.transport.reconnect(reset)
    }

    // --- keys and knobs --------------------------------------------------------

    /// Inject one front-panel key event (selector `0x13`).
    ///
    /// Fire and forget: the scope acknowledges receipt, not that the key had any effect.
    /// A key's meaning can depend on the open menu, and softkeys in particular are inert
    /// in menus that do not use them.
    pub fn press(&self, key: Key) -> Result<()> {
        // The state byte is a documented don't-care; the frame itself is the press.
        self.transport
            .transact(&[selector::KEY, key.id(), 0x01])
            .map(|_| ())
    }

    /// Press `key` `count` times, spaced by [`KEY_REPEAT_DELAY`].
    pub fn press_repeatedly(&self, key: Key, count: u32) -> Result<()> {
        for i in 0..count {
            if i > 0 {
                thread::sleep(KEY_REPEAT_DELAY);
            }
            self.press(key)?;
        }
        Ok(())
    }

    /// Turn a knob `steps` notches in `direction`.
    ///
    /// Directions follow the **value**, not the vendor key names — see [`Knob`]. Because
    /// each notch is one key event, this is subject to the same mailbox caveat as
    /// [`Device::press_repeatedly`]; verify against the settings block when it matters.
    pub fn turn(&self, knob: Knob, direction: Turn, steps: u32) -> Result<()> {
        self.press_repeatedly(knob.key(direction), steps)
    }

    /// Push a knob, if it has a push action. Pushing generally zeroes the knob's axis.
    ///
    /// Returns `Ok(false)` if this knob has no push key.
    pub fn push(&self, knob: Knob) -> Result<bool> {
        match knob.push_key() {
            Some(key) => self.press(key).map(|_| true),
            None => Ok(false),
        }
    }

    // --- settings --------------------------------------------------------------

    /// Read and decode the settings block (selector `0x01`).
    ///
    /// Retries a few times: a busy scope can transiently return a short ack frame instead
    /// of the block.
    pub fn read_settings(&self) -> Result<Settings> {
        let mut last = Error::Unexpected("settings never read".into());
        for attempt in 0..SETTINGS_ATTEMPTS {
            if attempt > 0 {
                thread::sleep(Duration::from_millis(150));
            }
            match self
                .transport
                .transact(&[selector::SETTINGS])
                .and_then(|payload| Settings::parse(&payload))
            {
                Ok(settings) => return Ok(settings),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    // --- screen ----------------------------------------------------------------

    /// Grab the scope's rendered screen (selector `0x20`).
    ///
    /// This is the only way to see what the instrument is drawing — including the
    /// logic-analyzer rows and menu state that the settings block does not expose.
    pub fn screenshot(&self) -> Result<Screenshot> {
        let first = self
            .transport
            .transact_with(&[selector::FRAMEBUFFER], SCREENSHOT_TIMEOUT, 1)?;
        let raw = self.collect_multiframe(first, SCREENSHOT_TIMEOUT)?;
        // A framebuffer transfer is large; clear any tail so the next command starts clean.
        self.transport.resync();
        Screenshot::from_rgb565(&raw).ok_or_else(|| {
            Error::Unexpected(format!(
                "framebuffer too short: {} bytes, want {}",
                raw.len(),
                screen::FRAMEBUFFER_BYTES
            ))
        })
    }

    // --- files -----------------------------------------------------------------

    /// Download a file from the scope's filesystem (selector `0x10`).
    ///
    /// The reply spans as many frames as needed, so this handles files far larger than the
    /// 64 KB single-frame cap — an exported deep-capture CSV of several megabytes included.
    ///
    /// # Short reads are not errors
    ///
    /// The protocol declares no total length, so this returns whatever the scope streamed.
    /// Some paths cannot be served at all — the running application binary `/dso_bin`
    /// answers with a single byte however long you wait — and a stream cut short mid-way
    /// yields a partial file rather than a failure. For transfers where truncation would
    /// matter, cross-check the length against the size from [`Device::list_dir`].
    pub fn download(&self, path: &str) -> Result<Vec<u8>> {
        let mut request = vec![selector::FILE_READ, 0x00];
        request.extend_from_slice(path.as_bytes());
        let first = self
            .transport
            .transact_with(&request, DOWNLOAD_TIMEOUT, 2)?;
        self.collect_multiframe(first, DOWNLOAD_TIMEOUT)
    }

    /// List a directory on the scope, via the shell channel.
    ///
    /// There is no protocol selector for this — it runs `ls -la` and parses the output.
    pub fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let output = self.shell(&format!("ls -la {path}"))?;
        Ok(files::parse_ls(&output))
    }

    // --- shell -----------------------------------------------------------------

    /// Run a shell command on the scope's embedded Linux and return its stdout.
    ///
    /// # Safety
    ///
    /// This executes **as root** on the instrument. Commands are screened by
    /// [`shell::check_command`], which refuses obviously destructive programs and
    /// redirection outside the removable card, but the channel is not a sandbox — treat it
    /// as read-only. A command that stalls the acquisition application will trip the
    /// scope's watchdog and reboot it.
    pub fn shell(&self, command: &str) -> Result<String> {
        shell::check_command(command)?;

        let sequence = self.shell_sequence.fetch_add(1, Ordering::Relaxed);
        let marker = shell::marker_for(sequence);
        let wrapped = shell::wrap_command(command, &marker);

        let mut request = vec![selector::SHELL];
        request.extend_from_slice(wrapped.as_bytes());

        // Replies can lag one command behind, so re-issue until we see our own marker.
        // Safe because the guard keeps commands read-only, hence repeatable.
        let mut last = String::new();
        for _ in 0..SHELL_ATTEMPTS {
            self.transport.resync(); // drop any stale reply still queued
            last = self.shell_exchange(&request)?;
            if let Some(output) = shell::output_before_marker(&last, &marker) {
                return Ok(output.to_string());
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(Error::Unexpected(format!(
            "shell reply never carried marker {marker} (last {} bytes)",
            last.len()
        )))
    }

    /// One command-channel exchange: send, then gather frames until the scope goes quiet.
    ///
    /// Unlike the data channel, shell replies carry no subtype markers — output simply
    /// spans however many frames it needs, and "no more frames" is signalled by a read
    /// timing out.
    fn shell_exchange(&self, request: &[u8]) -> Result<String> {
        let first =
            self.transport
                .transact_raw(protocol::LEADER_CMD, request, SHELL_TIMEOUT, 1)?;

        let mut text = Vec::new();
        text.extend_from_slice(strip_shell_frame(&first)?);
        while let Ok(frame) = self.transport.recv_raw(SHELL_TAIL_TIMEOUT) {
            match strip_shell_frame(&frame) {
                Ok(chunk) => text.extend_from_slice(chunk),
                Err(_) => break,
            }
        }
        // The scope's userland is byte-oriented; keep whatever it sent rather than failing
        // a whole listing on one stray byte.
        Ok(String::from_utf8_lossy(&text).into_owned())
    }

    // --- shared plumbing -------------------------------------------------------

    /// Collect a data-channel multi-frame reply.
    ///
    /// The scope streams DATA frames (subtype `0x01`) terminated by an END frame (subtype
    /// `0x02`). **The end marker must be consumed** or the next read starts mid-stream.
    fn collect_multiframe(&self, first: Vec<u8>, timeout: Duration) -> Result<Vec<u8>> {
        const DATA_OFFSET: usize = 2; // selector echo + subtype
        let mut frame = first;
        let mut data = Vec::new();
        // Anything other than a DATA frame — END, NODATA, or no subtype — ends the reply.
        while let Some(subtype::DATA) = frame.get(1).copied() {
            data.extend_from_slice(&frame[DATA_OFFSET..]);
            match self.transport.recv(timeout) {
                Ok(next) => frame = next,
                Err(_) => break,
            }
        }
        Ok(data)
    }
}

/// Strip a command-channel frame down to its payload, dropping the `0x91` ack byte.
fn strip_shell_frame(frame: &[u8]) -> Result<&[u8]> {
    let payload = protocol::payload_of(frame)?;
    Ok(match payload.first() {
        Some(&byte) if byte == (selector::SHELL | 0x80) => &payload[1..],
        _ => payload,
    })
}
