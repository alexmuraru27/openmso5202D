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

/// Attempts to grab a full framebuffer before giving up. A one-behind reply (a buffered
/// settings block, whose second byte can look like a DATA subtype) or a momentarily busy
/// scope yields a short/garbled grab; Python's `_grab_fb` retries up to four times with a
/// resync around each, and so do we.
const FRAMEBUFFER_ATTEMPTS: u32 = 4;

/// Settle after a resync before re-grabbing the framebuffer (Python's `time.sleep(0.4)`).
const FRAMEBUFFER_RETRY_SETTLE: Duration = Duration::from_millis(400);

/// Timeout for a waveform acquire. The `0x02` reply can take a moment, but once it has
/// answered the data frames arrive within a short window or not at all (the scope dropped
/// them — typically because a knob is being turned), so the follow-up frames use a much
/// shorter timeout to fail fast instead of hanging.
const WAVEFORM_TIMEOUT: Duration = Duration::from_millis(2000);

/// Timeout for the follow-up data frames of a waveform acquire (`min(timeout, 150)` in
/// `mso5202d.py`).
const WAVEFORM_TAIL_TIMEOUT: Duration = Duration::from_millis(150);

/// How many times to retry a waveform acquire that comes back empty (a resync happens
/// between attempts). Matches the Python `read_waveform` default.
const WAVEFORM_ATTEMPTS: u32 = 3;

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

    /// Discard anything still queued on the link so the next operation starts from a clean
    /// frame boundary.
    ///
    /// Worth doing at the head of a multi-step operation. A previous run — or a screen grab
    /// that was cut short — can leave frames on the endpoint, and because those are valid
    /// frames they pass the transport's checks and get handed to whichever command reads
    /// next, surfacing as a nonsense reply to an unrelated request.
    pub fn clear_link(&self) {
        self.transport.resync();
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

    /// Inject one front-panel key event with an **explicit edge/state byte**.
    ///
    /// [`Device::press`] always sends state `0x01`, which the firmware treats as a
    /// don't-care for almost every key. The store-depth softkey (F5 in the Acquire menu) is
    /// the exception: it advances one position per *edge*, so its walk has to send
    /// alternating `0x01` (press) / `0x00` (release) states, one step each. This is the
    /// primitive that lets it. (Python: the depth walk's `sc.transact([0x13, 5, edge])`.)
    pub fn key_edge(&self, key: Key, state: u8) -> Result<()> {
        self.transport
            .transact(&[selector::KEY, key.id(), state])
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
                // A payload of the wrong SHAPE means the stream is misaligned, not that the
                // scope was busy — typically a leftover framebuffer frame, which is a
                // perfectly valid `0x53` frame and so sails through `verify` and the
                // transport's own retry. Drain before retrying: a framebuffer is ~75 such
                // frames, so retrying without a resync just reads the next stale one and
                // burns the whole attempt budget on the same burst.
                self.transport.resync();
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

    // --- waveform acquire ------------------------------------------------------

    /// Acquire one screen-buffer sample block for an analog channel (selector `0x02`).
    ///
    /// `ch`: 0 = CH1, 1 = CH2. The channel is chosen by the acquire **value byte**
    /// (`02 01 <ch>`), not by any settings field — verified on hardware: with CH2's probe
    /// disconnected, `02 01 00` returns CH1's square wave and `02 01 01` returns CH2's flat
    /// line. Returns the raw sample bytes (two's-complement signed int8 per sample), or an
    /// **empty** vec when the channel is hidden (a disabled channel answers with no data) or
    /// the read is disrupted. Mirrors `Scope.read_waveform` in `mso5202d.py`.
    ///
    /// The scope wants a `12 01 00` latch written first (the vendor app pulses `0x12` around
    /// every refresh), then answers `02 01 <ch>` with a size frame, a data frame, and an end
    /// marker. The data frame is `echo | 0x01 | src | samples…`, so the samples start at
    /// **offset 3** — one past the file/framebuffer data offset, because of the extra source
    /// byte.
    ///
    /// Beware the one-deep channel pipeline: the FIRST acquire after switching `ch` returns
    /// the PREVIOUS channel's block, so a caller comparing channels must read twice and keep
    /// the second (see [`Device::channel_has_data`]).
    pub fn read_waveform(&self, ch: u8) -> Result<Vec<u8>> {
        // selector echo + subtype + source byte — one more than a file/framebuffer frame.
        const DATA_OFFSET: usize = 3;
        for attempt in 0..WAVEFORM_ATTEMPTS {
            if attempt > 0 {
                self.transport.resync();
            }
            match self.acquire_once(ch, DATA_OFFSET) {
                Ok(data) if !data.is_empty() => return Ok(data),
                Ok(_) => continue, // empty — retry after a resync
                Err(_) => continue,
            }
        }
        Ok(Vec::new())
    }

    /// One acquire attempt: latch, request the channel, and collect its data frame.
    fn acquire_once(&self, ch: u8, data_offset: usize) -> Result<Vec<u8>> {
        // Run-latch. The inner transactions use no retries so retrying is governed above.
        self.transport
            .transact_with(&[0x12, 0x01, 0x00], WAVEFORM_TIMEOUT, 0)?;
        let first = self
            .transport
            .transact_with(&[0x02, 0x01, ch], WAVEFORM_TIMEOUT, 0)?;

        let mut frame = first;
        let mut data = Vec::new();
        // Walk the size / data / end frames. Only the data frame carries samples; keep it.
        for _ in 0..5 {
            match frame.get(1).copied() {
                Some(subtype::DATA) => {
                    data = frame.get(data_offset..).unwrap_or(&[]).to_vec();
                }
                Some(subtype::END) => break,
                _ => {}
            }
            match self.transport.recv(WAVEFORM_TAIL_TIMEOUT) {
                Ok(next) => frame = next,
                Err(_) => break,
            }
        }
        Ok(data)
    }

    /// Whether analog channel `ch` (0 = CH1, 1 = CH2) is actually serving data.
    ///
    /// The reliable test of a channel being on is that its acquire returns samples — the
    /// `VERT-CHx-DISP` settings field is decoupled from the real acquisition and can lag or
    /// mislead. Double-reads to defeat the one-deep channel pipeline (the first read after a
    /// switch returns the previous channel), keeping the second. Only meaningful at 4 K with
    /// the scope running. Mirrors `_channel_enabled` in `mso5202d_plot.py`.
    pub fn channel_has_data(&self, ch: u8) -> Result<bool> {
        let _ = self.read_waveform(ch)?; // discard — pipeline returns the previous channel
        Ok(!self.read_waveform(ch)?.is_empty())
    }

    // --- screen ----------------------------------------------------------------

    /// Grab the scope's rendered screen (selector `0x20`).
    ///
    /// This is the only way to see what the instrument is drawing — including the
    /// logic-analyzer rows and menu state that the settings block does not expose.
    pub fn screenshot(&self) -> Result<Screenshot> {
        // Faithful port of `_grab_fb`: retry the whole grab, resyncing around each attempt,
        // and accept only a full-size screen. A single grab can come back short — a stale
        // one-behind reply gets read as the framebuffer, or the scope is momentarily busy —
        // so one attempt is not enough.
        for attempt in 0..FRAMEBUFFER_ATTEMPTS {
            // Start EVERY grab from a clean endpoint. The framebuffer is ~768 KB across ~75
            // frames; a single leftover frame — a previous grab's tail, or a stale
            // settings/shell reply whose bytes look like a framebuffer frame — gets read as
            // this grab's first frame and desyncs the whole transfer, and a retry issued
            // while the previous 768 KB is still in flight cascades into the following
            // commands (the `bad leader` we saw bleeding into the next shell `ls`). Resyncing
            // before the request is what keeps each grab self-contained.
            self.transport.resync();
            if attempt > 0 {
                thread::sleep(FRAMEBUFFER_RETRY_SETTLE);
            }
            let first = match self
                .transport
                .transact_with(&[selector::FRAMEBUFFER], SCREENSHOT_TIMEOUT, 1)
            {
                Ok(frame) => frame,
                Err(_) => continue, // resync at the top of the next attempt cleans up
            };
            let raw = self.collect_multiframe(first, SCREENSHOT_TIMEOUT)?;
            // A framebuffer transfer is large; clear any tail so the next command is clean.
            self.transport.resync();
            if let Some(shot) = Screenshot::from_rgb565(&raw) {
                return Ok(shot);
            }
            // Short/garbled (a stale frame, or the scope busy) — retry from a clean start.
        }
        // Give up, but not while leaving the transfer half-read: a framebuffer is ~75 frames
        // and any left queued would be picked up by the NEXT command as its own reply (a
        // 10210-byte "settings" payload, say). Failing must not poison the link.
        self.transport.resync();
        Err(Error::Unexpected(format!(
            "framebuffer grab did not return a full {}-byte screen after {FRAMEBUFFER_ATTEMPTS} attempts",
            screen::FRAMEBUFFER_BYTES
        )))
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
        self.download_with(path, |_| {})
    }

    /// [`Device::download`], reporting bytes received so far as the transfer proceeds.
    ///
    /// The reply carries no declared length, so `on_progress` receives only the running
    /// count — a caller wanting a percentage supplies the expected size itself, typically
    /// from [`Device::list_dir`].
    pub fn download_with(&self, path: &str, mut on_progress: impl FnMut(u64)) -> Result<Vec<u8>> {
        let mut request = vec![selector::FILE_READ, 0x00];
        request.extend_from_slice(path.as_bytes());
        let first = self
            .transport
            .transact_with(&request, DOWNLOAD_TIMEOUT, 2)?;

        const DATA_OFFSET: usize = 2; // selector echo + subtype
        // The stream always finishes with an END frame — it never just stops — so a recv
        // that times out mid-transfer is a transient gap, not the end. Retry a few times
        // before giving up; a single hiccup would otherwise silently truncate a multi-
        // megabyte file and only surface downstream as a "truncated" size mismatch.
        const RECV_RETRIES: usize = 4;
        let mut frame = first;
        let mut data = Vec::new();
        while let Some(subtype::DATA) = frame.get(1).copied() {
            data.extend_from_slice(&frame[DATA_OFFSET..]);
            on_progress(data.len() as u64);
            let mut next = None;
            for _ in 0..RECV_RETRIES {
                if let Ok(received) = self.transport.recv(DOWNLOAD_TIMEOUT) {
                    next = Some(received);
                    break;
                }
            }
            match next {
                Some(received) => frame = received,
                None => break,
            }
        }
        // A large transfer can leave a tail queued on the endpoint. Left there, the next
        // command reads it instead of its own reply — which showed up as a second
        // back-to-back download returning zero bytes, because the stale end-marker was
        // taken for that download's first frame.
        self.transport.resync();
        Ok(data)
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

        // Re-issue until we see our own marker. Two different things are being ridden out
        // here, and BOTH have to be retried rather than propagated:
        //
        //  * the reply can lag one command behind (wrong marker), and
        //  * the exchange can fail outright because a leftover **data-channel** frame was
        //    read where the `0x43` reply belonged (`bad leader 0x53`) — a big `0x53`
        //    transfer just before this, like a multi-megabyte file read, can leave one
        //    queued.
        //
        // The resync at the top of each attempt is what clears either, so a transport error
        // must not short-circuit the loop. Re-sending is safe because the guard keeps
        // commands read-only, hence repeatable.
        let mut last = String::new();
        let mut failure: Option<Error> = None;
        for _ in 0..SHELL_ATTEMPTS {
            self.transport.resync(); // drop any stale reply still queued
            match self.shell_exchange(&request) {
                Ok(text) => {
                    last = text;
                    if let Some(output) = shell::output_before_marker(&last, &marker) {
                        let output = output.to_string();
                        // Drain any trailing/duplicate 0x43 reply the scope is still
                        // dribbling before handing back to the data channel — otherwise the
                        // next 0x53 transact reads a stale command-channel frame (the
                        // mirror-image `bad leader 0x43` desync).
                        self.transport.resync();
                        return Ok(output);
                    }
                }
                Err(e) => failure = Some(e),
            }
            thread::sleep(Duration::from_millis(50));
        }
        Err(failure.unwrap_or_else(|| {
            Error::Unexpected(format!(
                "shell reply never carried marker {marker} (last {} bytes)",
                last.len()
            ))
        }))
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

    /// Collect the framebuffer's multi-frame reply, a faithful port of `_grab_fb`'s inner
    /// loop.
    ///
    /// The scope streams DATA frames (subtype `0x01`) terminated by an END frame (subtype
    /// `0x02`). Crucially, anything that is **neither** — a SIZE frame, or a stale one-behind
    /// frame a race slipped in front — is **skipped**, and reading continues, rather than
    /// stopping the collection early. Stopping early was the bug: it left the remaining
    /// ~768 KB of framebuffer frames queued on the endpoint, and those `0x53` frames then bled
    /// into the following shell (`0x43`) command as a `bad leader` desync. The loop is bounded
    /// (Python's `range(2000)`) so a stream that never ends cannot spin forever.
    fn collect_multiframe(&self, first: Vec<u8>, timeout: Duration) -> Result<Vec<u8>> {
        const DATA_OFFSET: usize = 2; // selector echo + subtype
        const MAX_FRAMES: usize = 2000;
        let mut frame = first;
        let mut data = Vec::new();
        for _ in 0..MAX_FRAMES {
            match frame.get(1).copied() {
                Some(subtype::DATA) => data.extend_from_slice(&frame[DATA_OFFSET..]),
                Some(subtype::END) => break,
                _ => {} // SIZE / stale frame — skip and keep reading until the END marker
            }
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
