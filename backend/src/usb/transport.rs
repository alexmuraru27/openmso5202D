//! The low-level USB driver: connect / reconnect / reset, interface binding, and the
//! transaction primitive.
//!
//! This is the Rust port of the transport half of the Python `Scope` class. The single
//! quirk that shapes the whole design: **the device only replies to an OUT command if a
//! bulk IN read is already pending when the command is written**. libusb's synchronous
//! API blocks on a read, so [`Transport::transact`] spawns a short-lived reader thread,
//! waits until it is about to post the IN read, waits [`TRANSACT_POST`] more for the URB
//! to actually reach the kernel, then writes the OUT frame. Below ~12 ms the write races
//! the read and the device goes silent.

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rusb::{DeviceHandle, GlobalContext};

use crate::error::{Error, Result};
use crate::protocol::{self, EP_IN, EP_OUT, INTERFACE, PID, VID};

/// Margin between the reader thread posting its IN read and us writing the OUT command.
/// Measured reliable with headroom; latency is USB-round-trip-limited below this, so
/// lowering it does not help. (Python: `TRANSACT_POST_S = 0.015`.)
pub const TRANSACT_POST: Duration = Duration::from_millis(15);

/// Default per-transaction timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(3000);

/// Default number of retries (a fresh resync happens between attempts).
pub const DEFAULT_RETRIES: u32 = 2;

/// Chunk size for bulk IN reads. The scope dribbles frames; we read in large chunks and
/// buffer whatever arrives.
const READ_CHUNK: usize = 65536;

type Handle = DeviceHandle<GlobalContext>;

/// Owns the USB connection to the scope and serialises access to it.
///
/// Cloning is intentionally not provided: exactly one `Transport` should own the device
/// at a time. Wrap it in a `Mutex`/actor if you need to share it across threads.
pub struct Transport {
    /// Shared with the reader thread for the duration of a single transaction.
    handle: Arc<Handle>,
    /// Persistent receive buffer — leftover bytes from one frame stay aligned for the
    /// next. `Arc<Mutex<…>>` so the reader thread can fill it; contention is nil because
    /// only one read path is ever active at once.
    rx: Arc<Mutex<Vec<u8>>>,
}

impl Transport {
    /// Open and bind the scope. If `reset` is true, issue a USB port reset first (matches
    /// the Python default) — this clears a wedged device but disturbs the scope's own USB
    /// host controller, so callers that drive the SD card path should pass `false`.
    pub fn open(reset: bool) -> Result<Self> {
        let handle = Self::open_handle(reset)?;
        Ok(Self {
            handle,
            rx: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Tear down and re-establish the connection from scratch (find → detach → claim →
    /// clear_halt), discarding any buffered bytes. Use after an unrecoverable USB error.
    pub fn reconnect(&mut self, reset: bool) -> Result<()> {
        self.handle = Self::open_handle(reset)?;
        self.rx.lock().unwrap().clear();
        Ok(())
    }

    /// Perform the full open recipe and return a ready-to-use handle.
    ///
    /// Order matters (learned from hardware): detach the auto-bound `cdc_subset` kernel
    /// driver → optionally port-reset and reopen (a reset re-enumerates the device) →
    /// detach again → claim interface 0 → clear both bulk endpoints.
    fn open_handle(reset: bool) -> Result<Arc<Handle>> {
        if reset {
            // Reset on a first, throwaway handle, then reopen — a port reset invalidates
            // the handle on re-enumeration.
            if let Some(h) = rusb::open_device_with_vid_pid(VID, PID) {
                Self::detach(&h);
                let _ = h.reset();
                drop(h);
                thread::sleep(Duration::from_millis(1000));
            }
        }

        let handle = rusb::open_device_with_vid_pid(VID, PID).ok_or(Error::NotFound)?;
        Self::detach(&handle);
        handle.claim_interface(INTERFACE)?;
        // Non-fatal: a fresh device may not be halted.
        let _ = handle.clear_halt(EP_OUT);
        let _ = handle.clear_halt(EP_IN);
        Ok(Arc::new(handle))
    }

    /// Detach the kernel driver (Linux auto-binds `cdc_subset` to this VID:PID). No-op
    /// on platforms/states where nothing is attached.
    fn detach(handle: &Handle) {
        if let Ok(true) = handle.kernel_driver_active(INTERFACE) {
            let _ = handle.detach_kernel_driver(INTERFACE);
        }
    }

    // --- transaction primitive -------------------------------------------------

    /// Send `payload` on the data channel and return the reply's **verified** payload
    /// (`selector_echo | subtype | data…`), retrying (with a resync between attempts).
    ///
    /// Uses the default timeout unless overridden with [`Transport::transact_with`].
    pub fn transact(&self, payload: &[u8]) -> Result<Vec<u8>> {
        self.transact_with(payload, DEFAULT_TIMEOUT, DEFAULT_RETRIES)
    }

    /// [`Transport::transact`] with an explicit timeout and retry count.
    pub fn transact_with(&self, payload: &[u8], timeout: Duration, retries: u32) -> Result<Vec<u8>> {
        let frame = self.transact_raw(protocol::LEADER_DATA, payload, timeout, retries)?;
        protocol::verify(&frame).map(<[u8]>::to_vec)
    }

    /// Send `payload` under an explicit `leader` and return the **complete raw frame**
    /// (leader, length, payload and checksum included).
    ///
    /// This is the leader-generic primitive both channels build on: the data channel adds
    /// checksum verification on top, while the command channel (`0x43`) parses by length
    /// only, since its replies carry no checksum we can rely on.
    pub fn transact_raw(
        &self,
        leader: u8,
        payload: &[u8],
        timeout: Duration,
        retries: u32,
    ) -> Result<Vec<u8>> {
        let mut last: Option<Error> = None;
        for _ in 0..=retries {
            match self.transact_once(leader, payload, timeout) {
                Ok(frame) => return Ok(frame),
                Err(e) => {
                    last = Some(e);
                    self.resync(); // clear desync before retrying
                }
            }
        }
        Err(last.unwrap_or(Error::Timeout(timeout)))
    }

    /// One attempt: post the IN read on a reader thread, wait the margin, write the OUT
    /// frame, then collect the reader's result.
    fn transact_once(&self, leader: u8, payload: &[u8], timeout: Duration) -> Result<Vec<u8>> {
        let handle = Arc::clone(&self.handle);
        let rx = Arc::clone(&self.rx);
        let (ready_tx, ready_rx) = mpsc::channel::<()>();

        let reader = thread::spawn(move || {
            // Signal that we are about to post the bulk IN read, then block on it.
            let _ = ready_tx.send(());
            Self::recv_frame(&handle, &rx, timeout)
        });

        // Wait for the reader to be up, then give the IN URB time to reach the kernel
        // before racing it with the OUT write.
        let _ = ready_rx.recv_timeout(Duration::from_millis(500));
        thread::sleep(TRANSACT_POST);

        let frame = protocol::build_with(leader, payload);
        let write_res = self
            .handle
            .write_bulk(EP_OUT, &frame, Duration::from_millis(2000));

        let recv_res = reader.join().expect("reader thread panicked");

        match recv_res {
            Ok(f) => Ok(f), // prefer the received frame even if the write reported an error
            Err(e) => Err(match write_res {
                Err(w) => Error::Usb(w),
                Ok(_) => e,
            }),
        }
    }

    /// Read one complete frame and return its **verified** data-channel payload.
    ///
    /// Higher layers use this to drain the follow-up frames of a multi-frame reply (the
    /// first frame comes back from [`Transport::transact`]; the rest via this).
    pub fn recv(&self, timeout: Duration) -> Result<Vec<u8>> {
        let frame = self.recv_raw(timeout)?;
        protocol::verify(&frame).map(<[u8]>::to_vec)
    }

    /// Read one complete frame and return it raw (leader and checksum included).
    pub fn recv_raw(&self, timeout: Duration) -> Result<Vec<u8>> {
        Self::recv_frame(&self.handle, &self.rx, timeout)
    }

    /// Core receive: fill the persistent buffer until one whole frame is present and split
    /// it off. Returns the frame verbatim; validation is the caller's choice.
    fn recv_frame(handle: &Handle, rx: &Mutex<Vec<u8>>, timeout: Duration) -> Result<Vec<u8>> {
        let mut buf = rx.lock().unwrap();
        let mut scratch = vec![0u8; READ_CHUNK];

        // Need the 3-byte header to know the frame length.
        while buf.len() < 3 {
            let n = handle.read_bulk(EP_IN, &mut scratch, timeout)?;
            if n == 0 {
                return Err(Error::Timeout(timeout));
            }
            buf.extend_from_slice(&scratch[..n]);
        }
        let total = protocol::frame_total_len(&buf).unwrap();
        while buf.len() < total {
            let n = handle.read_bulk(EP_IN, &mut scratch, timeout)?;
            if n == 0 {
                return Err(Error::Timeout(timeout));
            }
            buf.extend_from_slice(&scratch[..n]);
        }

        Ok(buf.drain(..total).collect())
    }

    /// Discard buffered bytes and drain the endpoint so the next frame starts on a clean
    /// boundary. Called after a timeout or bad frame — an interrupted large read (file or
    /// framebuffer) can leave hundreds of KB queued that would otherwise cascade into
    /// repeated failures. Bounded so a desync costs ~1–2 s at worst.
    pub fn resync(&self) {
        self.rx.lock().unwrap().clear();
        let mut scratch = vec![0u8; READ_CHUNK];
        for _ in 0..64 {
            match self
                .handle
                .read_bulk(EP_IN, &mut scratch, Duration::from_millis(60))
            {
                Ok(0) => break,          // endpoint dry
                Ok(_) => continue,       // keep draining — scope dribbles frames
                Err(_) => break,         // timeout = nothing more pending
            }
        }
    }

    /// The USB bus and address the scope is currently at, e.g. for building a usbmon
    /// capture filter. `None` if the device vanished.
    pub fn bus_address(&self) -> Option<(u8, u8)> {
        let dev = self.handle.device();
        Some((dev.bus_number(), dev.address()))
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(INTERFACE);
    }
}
