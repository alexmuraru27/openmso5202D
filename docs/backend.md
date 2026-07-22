# Backend developer guide (`backend/`, crate `mso5202d`)

`backend/` is the Rust driver crate for the Hantek MSO5202D (USB `049f:505a`). It owns everything
between the USB bus and a caller that wants to say "reset the scope, put CH1 on 1 V/div, capture
512 K samples and give me the decoded SPI bytes": USB transport and framing, the 213-byte settings
block, front-panel key/knob operations, screen grabs, the scope's shell channel, exported-CSV
parsing, serial-protocol decoders, and a plan-based control layer that drives all of it closed-loop.
The wire format itself is documented separately in `docs/MSO5202D-protocol.md`; this document
describes the code. The crate is consumed by `frontend/src-tauri` and by the binaries in
`backend/src/bin/`.

## Module map

| Layer | Path | Owns |
|---|---|---|
| 3 | `backend/src/control/mod.rs` | Plan execution, op implementations, save/export flow |
| 3 | `backend/src/control/ops.rs` | The `Op` vocabulary and its labels |
| 3 | `backend/src/control/capture.rs` | `CaptureSpec`, `prepare`/`capture`/`deep_capture` |
| 3 | `backend/src/control/trigger.rs` | Trigger model + trigger-menu navigation |
| 3 | `backend/src/control/converge.rs` | Closed-loop primitives (`converge`, `cycle_until`, `open_menu`) |
| 3 | `backend/src/control/csv.rs` | CSV menu screen reading, `CsvSource`, WaveData filenames |
| 3 | `backend/src/control/progress.rs` | `ProgressEvent` / `StepState` / `ProgressSink` |
| 2 | `backend/src/device/mod.rs` | `Device` — one method per logical instrument operation |
| 2 | `backend/src/device/keys.rs` | `Key` (49 front-panel ids), `Knob`, `Turn` |
| 2 | `backend/src/device/files.rs` | `FileEntry`, `ls -la` parsing |
| 2 | `backend/src/device/screen.rs` | `Screenshot`, RGB565 → RGB8 |
| 2 | `backend/src/device/shell.rs` | `0x43` shell channel guard, marker wrapping |
| 1 | `backend/src/usb/transport.rs` | `Transport` — connect, transact, resync, USB logging |
| 0 | `backend/src/protocol/mod.rs` | Constants, `build`/`verify`, selector & subtype tables |
| 0 | `backend/src/settings/mod.rs` | `Settings`, field table, enums |
| 0 | `backend/src/settings/tables.rs` | `VB_TO_MV`, `TB_TO_NS`, `MENU_NAMES`, `ACQ_DEPTH_NAMES` |
| 0 | `backend/src/waveform.rs` | Exported-CSV parsing (`WaveformCsv`, `parse_csv`) |
| 0 | `backend/src/decoder/` | UART / SPI / I²C decoding on captured traces |
| — | `backend/src/error.rs` | `Error`, `Result` |
| — | `backend/src/logging.rs` | `tracing` file+console setup, log retention |

## Purpose and layering

`backend/src/lib.rs` states the layering in its rustdoc and it is enforced by convention: **each
layer depends only on the layer below it.**

- **Layer 0 — pure logic.** `protocol`, `settings`, `waveform`, `decoder`. No I/O, no `Device`, no
  `Transport`. Everything here is testable and runnable without an instrument, which is what makes
  the CSV corpus tests and the decoder tests hardware-free.
- **Layer 1 — USB transport.** `usb::Transport`. Owns the libusb handle, the connection recipe, the
  transaction primitive and the receive buffer. Knows about frames, not about oscilloscopes.
- **Layer 2 — device operations.** `device::Device`. Turns selectors into named instrument actions:
  press a key, turn a knob, read the settings, grab the screen, download a file, list a directory,
  run a shell command. Each method is one logical operation that performs its exchange and returns.
  Deliberately *not* here: closed-loop targeting, menu navigation, multi-step workflows, decoding.
- **Layer 3 — control.** `control`. Business logic as a plan of semantic operations, with read-back
  verification, menu navigation, failure policy and progress reporting.

Re-exports at the crate root (`backend/src/lib.rs`): `Device`, `FileEntry`, `Key`, `Knob`,
`Screenshot`, `Turn`, `Error`, `Result`, `Settings`, `StoreDepth`, `TrigState`, `Transport`, `PID`,
`VID`, `execute`, `CaptureSpec`, `Context`, `Op`, `ProgressEvent`, `ProgressSink`, `StepState`.

## Module by module

### `protocol` — `backend/src/protocol/mod.rs`

Constants and framing. `VID`/`PID`, `EP_IN`/`EP_OUT`/`INTERFACE`, the two frame leaders
`LEADER_DATA` (`0x53`) and `LEADER_CMD` (`0x43`), the `selector` submodule (`SETTINGS`, `FILE_READ`,
`KEY`, `FRAMEBUFFER`, `SHELL`) and the `subtype` submodule (`SIZE`, `DATA`, `END`, `NODATA`).

- `build(payload)` / `build_with(leader, payload)` — wrap a payload in a frame.
- `verify(frame)` — leader + length + checksum check, returns the payload. Used for `0x53`.
- `payload_of(frame)` — length-only extraction, used for `0x43` replies which carry no checksum
  worth trusting.
- `frame_total_len(head)` — crate-internal, lets the receiver size a frame from its first 3 bytes.

### `settings` — `backend/src/settings/mod.rs`, `tables.rs`

The 213-byte block (`SETTINGS_LEN`), laid out as `SETTINGS_PARAMS`: `(name, width)` pairs in wire
order with no padding, widths summing to `SETTINGS_LEN`. Multi-byte fields are little-endian;
`SIGNED_FIELDS` lists the two's-complement ones (positions, trigger level, slope thresholds,
`HORIZ-TRIGTIME`, LA thresholds).

`Settings` stores the raw bytes and decodes on demand:

- generic access: `field(name) -> Option<u64>`, `field_signed`, `field_auto` (applies
  `SIGNED_FIELDS`), `raw()`.
- named accessors: `trig_state`, `menu_id`, `menu_name`, `channel_shown`, `volts_per_div_mv`,
  `input_volts_per_div_mv`, `probe`, `coupling`, `channel_position`, `time_per_div_ns`,
  `acquisition_time_per_div_ns`, `sample_interval_ns`, `store_depth`, `trigger_position`,
  `trigger_level_mv`, `la_enabled`, `la_channel_mask`.
- `Settings::parse` accepts either the bare 213 bytes or the 214-byte reply payload with its `0x81`
  echo.

Enums: `TrigState` (with `is_stopped()` covering **both** `Stop` and `SingleCaptured` — a completed
single sequence is stopped, and treating it as running makes a stop request start the scope),
`StoreDepth` (`K4`/`K40`/`K512`/`M1`, gapped wire codes, `code()` round-trips), `Probe` (with
`factor()`), `Coupling`.

Two scale distinctions matter: `volts_per_div_mv` is the position on the scope's **ladder** (what the
knob steps and what convergence targets), while `input_volts_per_div_mv` multiplies it by the probe
attenuation and is what a signal actually measures. `trigger_level_mv` uses the input scale and
returns `None` for non-CH1/CH2 sources.

`tables.rs` holds `VB_TO_MV` (11 entries, 2 mV…5 V), `TB_TO_NS` (32 entries, 2 ns…40 s),
`ACQ_DEPTH_NAMES`, `MENU_NAMES` and the `lookup` helper.

**Policy: the block is read-only.** Nothing in the crate writes it; see "Conventions" below.

### `waveform` — `backend/src/waveform.rs`

Parses the CSVs the scope writes to the card, which is the only route to a record longer than the
screen buffer. `parse_csv(text) -> Result<WaveformCsv>`; `WaveformCsv` carries `time_s`, `dt_s`
(median step, taken from the data rather than the header), `size`, `timebase_ps`, and then either
`volts` + `volts_per_div_mv` (analog) or `words` + `threshold_mv` (logic), distinguished by
`is_logic()`. Parsing is tolerant: headers are matched by pattern anywhere in the preamble and
unparseable rows are skipped rather than failing the file.

### `decoder` — `backend/src/decoder/`

Pure logic, runs on saved captures. `Event { start, end, value, ok, kind }`, `Kind`
(`Byte`/`Address`/`Start`/`RepeatedStart`/`Stop`), `Event::text()` for scope-style annotation, and
`values(events)` to strip markers.

- **`common.rs`** — the shared front end. `threshold_volts` (→ `threshold_local`) digitises against a
  sliding local envelope with a Schmitt band, falling back to `threshold_global` when there are too
  few transitions; `sliding_extreme` keeps the envelope O(n) on deep records. Then `edges`,
  `min_pulse`, `idle_level`, `refine_period` (hierarchical period search scored by
  `grid_concentration`, polished by `fit_grid` only if it stays in the search's basin),
  `sample_grid`/`sample_cell` (majority vote over each cell's middle), `both_ways` (decode forwards
  and reversed, keep the better), plus `percentile`, `round_half_even` (NumPy tie-breaking) and
  `ramp_ratio` for scoring.
- **`uart.rs`** — `decode(trace, UartOptions)`. Options: `sample_interval_ns`, `baud`, `bits`,
  `parity` (`Parity`), `stops`, `idle` (`Idle::High`/`Low`/`Auto`), `both_ways`. Decodes on the bit
  grid rather than by hunting start edges, and picks between fixed-offset tiling and a greedy resync
  walk by whichever validates more frames. `FrameLayout` describes cell positions forwards and
  reversed.
- **`spi.rs`** — `decode(clock, data, chip_select, clock_analog, SpiOptions)`. Options: `cpol`,
  `cpha`, `msb_first`, `bits`, `word_gap`, `max_missed`, `anchor` (`Anchor::Start`/`End`/`Auto`),
  `auto_mode`. `detect_sample_rising` infers the sampling edge from the signals; `gap_has_pulse`
  uses the raw analog clock to tell a missed clock edge from a real inter-word idle.
- **`i2c.rs`** — `decode(scl, sda, Anchor)`. Forward decoding from START conditions
  (`decode_forward`, handling repeated START, 7- and 10-bit addressing, ACK/NACK) with
  `decode_end_anchored` as the fallback when the capture missed the START.

### `usb` — `backend/src/usb/transport.rs`

`Transport` owns the `rusb` handle and a persistent receive buffer, and is intentionally not
`Clone`: one owner at a time.

- `Transport::open(reset)` runs the connection recipe in `open_handle`: detach the auto-bound
  `cdc_subset` kernel driver → optionally port-reset and reopen → detach again → claim interface 0 →
  `clear_halt` on both endpoints. `reconnect(reset)` releases the interface first, then repeats it.
- `transact(payload)` / `transact_with(payload, timeout, retries)` — data channel, returns the
  verified payload. `transact_raw(leader, payload, timeout, retries)` — leader-generic, returns the
  whole frame; used by the shell channel.
- The device only replies if a bulk IN read is already pending when the OUT frame is written, so
  `transact_once` spawns a short-lived reader thread, waits for it to signal, sleeps `TRANSACT_POST`
  (15 ms) so the IN URB reaches the kernel, then writes.
- `transact_validated` is the shared retry loop: validate **inside** the loop via `validate_reply`
  (full `protocol::verify` for `0x53`; leader match only for `0x43`), and `resync()` between
  attempts. `validate_reply` deliberately does not check the `selector | 0x80` echo, because a
  `0x13` key press is not idempotent and must not be re-sent on a one-behind reply.
- `recv(timeout)` / `recv_raw(timeout)` read one further frame — how callers drain multi-frame
  replies. `resync()` clears the buffer and drains the endpoint (bounded to 64 chunks).
  `bus_address()` reports bus/address, e.g. for a usbmon filter.
- `Drop` releases the interface.

Constants: `TRANSACT_POST`, `DEFAULT_TIMEOUT` (3 s), `DEFAULT_RETRIES` (2).

### `device` — `backend/src/device/`

`Device` wraps a `Transport` plus a shell sequence counter.

- Connect: `Device::connect()` (with USB reset) and `Device::connect_without_reset()` — **use the
  latter for anything touching the SD card**, since a reset disturbs the scope's own USB host
  controller, which the card hangs off. Also `with_transport`, `transport()`, `reconnect(reset)`,
  `clear_link()`.
- Keys/knobs: `press(Key)`, `key_edge(Key, state)` (the store-depth softkey advances one position
  per *edge*, so its walk needs alternating `0x01`/`0x00`), `press_repeatedly(key, count)` spaced by
  `KEY_REPEAT_DELAY`, `turn(Knob, Turn, steps)`, `push(Knob) -> Result<bool>`.
- Settings: `read_settings()` — retries `SETTINGS_ATTEMPTS` times, **resyncing between attempts**,
  because a wrongly-shaped payload means a stale (perfectly valid) frame from another transfer was
  read, not that the scope was busy.
- Waveform: `read_waveform(ch)` (0 = CH1, 1 = CH2) writes the `12 01 00` latch then walks the
  size/data/end frames, samples starting at offset 3; returns an empty vec for a hidden channel.
  `channel_has_data(ch)` double-reads to defeat the one-deep channel pipeline (the first acquire
  after switching channel returns the *previous* channel).
- Screen: `screenshot() -> Screenshot`, retrying `FRAMEBUFFER_ATTEMPTS` times with a `resync()`
  before every attempt and after every transfer, accepting only a full `FRAMEBUFFER_BYTES` screen.
- Files: `download(path)`, `download_with(path, on_progress)` — multi-frame, no declared total
  length, so short reads are returned rather than erroring and callers cross-check against
  `list_dir`. `list_dir(path)` runs `ls -la` over the shell channel and parses it with
  `files::parse_ls`.
- Shell: `shell(command)` — guards with `shell::check_command`, wraps in a brace group with a unique
  marker (`shell::wrap_command`, `shell::marker_for`), and re-issues up to `SHELL_ATTEMPTS` times
  until the reply carries that marker (`shell::output_before_marker`), resyncing before each
  attempt and after success. Reply frames are stripped by `strip_shell_frame`.
- `collect_multiframe` is the shared framebuffer collector: DATA frames accumulate, END stops,
  anything else (SIZE, a stale frame) is **skipped rather than ending collection**, so the rest of
  the transfer is never left queued on the endpoint.

`keys.rs` enumerates all 49 `Key` values by their `/keyprotocol.inf` index (`Key::id()`). `Knob`
groups ± key pairs and push keys, expressed in terms of the **value**: `Turn::Down` on
`Knob::TimePerDiv` always means a faster timebase, whatever the vendor labelled the key.
`Knob::volts_per_div(ch)` / `Knob::position(ch)` look a knob up by channel.

`screen.rs`: `SCREEN_WIDTH`/`SCREEN_HEIGHT`/`FRAMEBUFFER_BYTES`, `Screenshot::from_rgb565`,
`width`/`height`/`rgb`/`pixel`.

`shell.rs`: `check_command` refuses any token whose basename is in `DESTRUCTIVE` and refuses output
redirection outside `WRITABLE_PREFIX` (`/mnt/udisk`). `cp`, `mkdir`, `touch` are allowed because
writing to the card is the intended export path. The guard is a safety net, not a sandbox — the
channel runs as root on the instrument.

### `control` — `backend/src/control/`

**The plan model.** A plan is plain data, `Vec<Op>`, so the step count and every label are known
before anything runs. `execute(&Context, &[Op])` iterates, emits progress, dispatches each op through
`run`, and **stops at the first failure** — continuing would acquire at settings the caller never
asked for. There is no interpreter: plans encode no control flow.

`Context` carries the `&Device`, the `&dyn ProgressSink`, an optional `&AtomicBool` cancel flag
(`Context::cancellable`, checked at step boundaries only, so the scope is never left half-configured),
the current `Step`, the `Outputs`, and a `Session`. `Context::advance(done, total)` reports
sub-progress; `outputs()` / `take_outputs()` retrieve results. `Outputs.files` is a `Vec<CapturedFile>`
(`source`, `name`, `size`, optional `data`, `path()`), queryable with `Outputs::file(source)`.
`Session.filelist_open` tracks whether the CSV file list is showing, because that changes how many
Save presses are needed.

**`ops.rs`** defines the vocabulary: `DefaultSetup`, `SetChannel`, `SetProbe`, `SetCoupling`,
`SetBandwidthLimit`, `SetInvert`, `SetVoltsPerDiv`, `SetTimePerDiv`, `SetTrigger`, `SetTriggerValue`,
`SetTriggerLevel`, `SetDepth`, `ArmSingle`, `WaitCaptured`, `SaveCsv`, `Download`, `ClearCard`. Every
op carries `label()`, at the altitude a user recognises ("Turning on CH1"); retries and menu
navigation are implementation detail inside an op and never appear as steps. Also `format_volts` and
`format_time`, which render values the way the scope displays them.

**`converge.rs`** is where the closed loop lives, because the settings block is read-only and every
key press can be dropped by the scope's single-slot key mailbox:

- `converge(device, knob, target, tolerance, read)` and `converge_within(…, max_steps, read)` — nudge
  and re-read, deciding direction from the read-back every iteration. A value that does not move is
  re-nudged `NONMOVE_RETRIES` times with increasing waits before being called an end stop; a `read`
  returning `None` is retried, because "not readable right now" is not "never readable".
- `cycle_until(device, key, ring_size, read, target)` — for ring settings (probe, depth, trigger
  type). A press that lands counts as a lap; a press that moves nothing counts as a stall and is
  re-pressed, so a dropped press cannot exhaust the ring budget.
- `open_menu(device, key, wanted)` — press until `CONTROL-MENUID` is one of `wanted`. Softkeys only
  acquire meaning from the open menu, so nothing is ever pressed blind.
- `SETTLE` (400 ms) and `MENU_SETTLE` (350 ms) are the shared waits.

**Op implementations in `control/mod.rs`.** `default_setup`, `set_channel` (the channel button is a
toggle, so state is read first — and checked against `channel_has_data`, not `VERT-CHx-DISP`),
`set_probe` and `set_channel_option` (both open the channel menu via `ensure_channel_menu` and
restore the display state afterwards, since the button that opens the menu also toggles the channel),
`set_volts_per_div`, `set_time_per_div`, `set_trigger_level`, `set_depth`/`depth_walk` (walks the
Acquire-menu F5 ring `DEPTH_RING` one **edge** at a time, polling `ACQURIE-STORE-DEPTH` after each,
self-correcting a no-op edge by flipping it; the whole walk retries `DEPTH_WALK_ATTEMPTS` times),
`arm_single`, `wait_captured` (forces a trigger once after `FORCE_AFTER`), `resume_run`,
`finalize_capture`, `save_csv`, `download`, `clear_card`.

The save flow is the most intricate part: `open_csv_menu` (idempotent), `select_source`
(framebuffer-verified, retried `SOURCE_ATTEMPTS` times), the one-or-two Save presses decided by
`Session.filelist_open`, `await_new_file` (picks the highest-numbered new WaveData file, then waits
for its size to stop changing; re-presses Save only after `SAVE_RETRY_GRACE`, because pressing during
the write corrupts the save and advances the Source radio), and `await_save_finished` (polls the
screen for the busy banner, during which the scope ignores keys). `list_wavedata` retries an empty
listing; `list_wavedata_if_reachable` treats a shell failure as "still busy" rather than "no files".
`save_timeout`, `csv_rows`, `expected_csv_bytes` and `CARD_PATH` support it.

**`csv.rs`** reads what the settings block does not expose. `CsvSource` (`Ch1`/`Ch2`/`La`, `ALL`,
`name()`), `selected_source(&Screenshot)` — identifies the selected radio as the **odd one out**
among three dots rather than matching a fixed accent colour, and returns `None` rather than guessing
when the group looks uniform — and `save_in_progress(&Screenshot)`. Plus `wavedata_files`,
`is_wavedata`, `wavedata_number`.

**`trigger.rs`** models the trigger and drives its menus. Types: `TriggerType`, `TriggerSource`,
`TriggerMode`, `TriggerCoupling`, `Polarity`, `Qualifier`, `VideoStandard`, `VideoSync`, `AlterType`,
`AlterChannel`, `TriggerSetup`, `Adjustable`. `TriggerSetup::matches` compares only the fields that
belong to the requested type, so a UI can carry settings for every type while only one is live.

- `read(&Settings) -> Option<TriggerSetup>` and `read_alter_channel` decode the current state.
- `apply(device, setup)` / `apply_reporting(device, setup, progress)` apply one. They no-op when
  `matches` already holds. The type is set first (it decides which page is open and therefore what
  every other softkey means), then source, polarity, mode/standard, coupling/sync, and — via a page
  turn — the qualifier. `FIELDS` is the progress denominator. Alter branches to
  `apply_alter_channel` per channel.
- Navigation: `goto_type` cycles the type softkey and then re-normalises the page; `open_trigger`
  presses the trigger key until a **first** page from `FIRST_PAGES` is open *and* `CONTROL-DISP-MENU`
  says the bar is visible; `open_second_page`, `open_alter_channel`, `confirm_menu`.
- Level: `level_for_convergence` (returns `None` under Alter unless `TRIG-SRC` is CH1, since the
  field is shared), `set_level`, `level_to_ground` (the level knob's push), `nudge_level`.
- Knob-only values: `nudge` and `set_value(device, kind, what, target)`. `set_value` learns the step
  from the read-back (`knob_step`) and fires presses in runs of up to `KNOB_BATCH` spaced by
  `KNOB_SETTLE` (60 ms), reading between runs — one read per run rather than per press.
  `hand_knob_the` navigates to the page that owns the parameter and selects its box.
- Softkey constants are named by function: `KEY_TYPE`, `KEY_SOURCE`, `KEY_POLARITY`, `KEY_MODE`,
  `KEY_COUPLING`, `KEY_PAGE`, `KEY_QUALIFIER`, `KEY_ALTER_CH1/CH2/BACK`. `Fn7` is never used.

**`capture.rs`** composes the two cohesive workflows. `ChannelSetup` (probe, coupling, bandwidth
limit, invert) and `CaptureSpec` (channels, depth, `volts_per_div_mv`, `timebase_ns`,
`trigger_position`, `trigger`, `trigger_values`, `channel_setup`, `reset`, `wait_trig_s`,
`delete_after`) produce two plans:

- `CaptureSpec::prepare_plan()` — Default Setup, then per channel display/probe/scale/coupling/
  bandwidth/invert, then timebase, then trigger and its knob-only values, then level (skipped for
  types where `has_level()` is false), then depth. Probe precedes scale because it multiplies what
  every volts figure means; the trigger follows the channels because a reset would undo it.
- `CaptureSpec::capture_plan()` — `ArmSingle`, `WaitCaptured`, one `SaveCsv` per source, then all the
  `Download`s (deferred so a multi-megabyte read never sits between Source changes), then optionally
  `ClearCard`. `CaptureSpec::sources()` orders CH1 before CH2 deterministically.

`prepare(&Context, &spec)` and `capture(&Context, &spec)` run them (capture always calls
`finalize_capture` afterwards, success or failure); `deep_capture` does both.
`deep_tdiv_for_bit(bit_ns, depth, target_samples)` computes the SEC/DIV that puts a wanted number of
deep samples on each bit.

**`progress.rs`** — `StepState` (`Started`, `Advanced { done, total }`, `Completed { elapsed_ms }`,
`Failed { error }`), `ProgressEvent { index, total, label, state }` with a `Display` impl,
`ProgressSink` (blanket-implemented for any `Fn(&ProgressEvent)`) and `SilentProgress`. There is
deliberately **no per-step weighting**: duration depends on how far the instrument is from the
target, not on the operation, so long steps report real sub-progress instead.

### `error.rs`

`Result<T> = std::result::Result<T, Error>` and `Error`: `NotFound`, `Framing(String)`,
`Timeout(Duration)`, `Usb(#[from] rusb::Error)`, `UnsafeCommand(String)`, `Cancelled`,
`Unexpected(String)`. `Framing` is a malformed frame; `Unexpected` is a well-formed answer we cannot
use, and is also what the control layer uses for "the scope would not do what was asked" (end stops,
menus that will not open, unreachable values).

### `logging.rs`

`init()` / `init_in(dir)` install a global `tracing` subscriber: DEBUG and above to a daily rolling
file under `logs/` (`DEFAULT_LOG_DIR`, prefix `openmso5202d.log`), WARN and above to stderr unless
`MSO_LOG` overrides it (`MSO_LOG=info` for the step trace, `MSO_LOG=trace` for every USB
transaction). Returns a `#[must_use]` `LogGuard` that must be kept alive for the whole program.
`prune_expired` deletes files older than `RETENTION_DAYS` (3) at startup. This installs a global
subscriber, so it belongs to binaries, not to library callers.

## Main flows

**Connect.**
`Device::connect()` → `Transport::open(true)` → `Transport::open_handle` (detach `cdc_subset` →
reset → reopen → detach → `claim_interface` → `clear_halt` ×2). Card-touching work uses
`Device::connect_without_reset()` instead.

**Read settings.**
`Device::read_settings()` → `Transport::transact(&[selector::SETTINGS])` →
`transact_validated` → `transact_once` (reader thread → `TRANSACT_POST` → `write_bulk` →
`recv_frame`) → `validate_reply` → `protocol::verify` → `Settings::parse`. On a wrongly-shaped
payload: `Transport::resync()`, wait, retry.

**Prepare.**
`capture::prepare(ctx, spec)` → `Device::clear_link()` → `execute(ctx, spec.prepare_plan())` → per op
`control::run` → e.g. `set_volts_per_div` → `converge::converge` → `Device::turn` →
`Device::read_settings` → repeat. `Op::SetDepth` goes `set_depth` → `depth_walk` →
`converge::open_menu`-style Acquire open → `Device::key_edge(Fn5, edge)` → `depth_now` poll.
`Op::SetTrigger` goes `trigger::apply_reporting` → `set_type`/`goto_type` → `converge::cycle_until`
per field.

**Capture and export.**
`capture::capture(ctx, spec)` → `execute(ctx, spec.capture_plan())`:
`arm_single` (`resume_run` if stopped → `Device::press(Key::Single)`) → `wait_captured` (poll
`TrigState::is_stopped`, `Key::ForceTrigger` once after `FORCE_AFTER`) → per source `save_csv`
(`open_csv_menu` → `select_source` → `Device::screenshot` + `csv::selected_source` → Save presses →
`await_new_file` → `await_save_finished`) → per source `download` (`Device::download_with` →
truncation check against the card's size) → optional `clear_card`. Finally `finalize_capture`
(resync, `resume_run(true)`, back out of the file list).

**Decode.**
`waveform::parse_csv(text)` → `WaveformCsv.volts` → `decoder::common::threshold_volts` →
`decoder::uart::decode` / `decoder::spi::decode` / `decoder::i2c::decode` → `Vec<Event>` →
`decoder::values(&events)`.

**File access.**
`Device::list_dir(path)` → `Device::shell("ls -la …")` → `shell::check_command` →
`shell::wrap_command` → `Transport::transact_raw(LEADER_CMD, …)` → `shell_exchange` →
`strip_shell_frame` → `shell::output_before_marker` → `files::parse_ls`. Then
`Device::download(path)` → `transact_with(&[FILE_READ, 0x00, path…])` → frame loop → `resync`.

## Cross-cutting concerns

**Errors.** Everything fallible returns `Result<T>`. Transport-level failures are retried inside
`transact_validated`; device-level shape failures are retried inside `read_settings`, `screenshot`,
`read_waveform` and `shell`; control-level failures stop the plan. Cancellation surfaces as
`Error::Cancelled`, only ever between ops.

**Progress.** Ops emit `StepState::Started` / `Completed` / `Failed` automatically from `execute`.
Long ops call `Context::advance(done, total)` where a real measure exists — bytes written to the card
in `await_new_file`, bytes transferred in `download`, files deleted in `clear_card`, fields settled in
`trigger::apply_reporting`. Any `Fn(&ProgressEvent)` is a valid `ProgressSink`.

**Logging.** `tracing` spans and events throughout: `execute` opens a `plan` span and a `step` span
per op, and logs `begin` *before* the work so a hang shows as a step that started and never
completed. `Transport` logs every transaction at `trace` and every failure/resync at `debug`.

**`MSO_USB_LOG` diff harness.** Setting `MSO_USB_LOG=1` (or `=stderr`, or a file path) makes
`transport::log_usb` record every OUT frame written, IN chunk read and DRN chunk drained by a resync,
each with a millisecond timestamp, byte length and hex head — so a Rust wire trace can be diffed
byte-for-byte and delay-for-delay against a Python one. Independent of the `tracing` setup.

**Closed-loop convergence.** `control::converge` is the single place that encodes "read, compare,
nudge, repeat" and the distinction between a dropped press and a genuine end stop. New settings
operations should be built on `converge`, `converge_within`, `cycle_until` and `open_menu` rather
than pressing keys directly.

## Binaries (`backend/src/bin/`)

Only `playground` is declared explicitly in `backend/Cargo.toml`; the rest are auto-discovered from
`src/bin/`. All of them need the scope attached, plus either the `70-mso5202d.rules` udev rule or
root. Each calls `logging::init()`, so every run leaves a trace in `logs/`.

| Binary | Purpose | Run |
|---|---|---|
| `playground.rs` | Scratch "hack main": prints the current settings, then runs a hand-written plan end to end. Edit it freely. | `cargo run -p mso5202d --bin playground` (or `pnpm playground`) |
| `capture_test.rs` | Hardware smoke test for `control::capture`: checks the depth cycles, each requested Source is selected and exported, and every export downloads and parses. Flags: `--depth 4k\|40k\|512k\|1m`, `--channels 1,2`, `--tb-ns N`, `--clear`. | `cargo run -p mso5202d --bin capture_test -- --depth 40k --channels 1,2 --tb-ns 2000` |
| `trigger_test.rs` | Round-trips a series of `TriggerSetup`s: applies each, reads it back, and also checks knob-only `set_value`, the Alter level, the no-op re-apply, and that Overtime refuses EXT. | `cargo run -p mso5202d --bin trigger_test` |
| `trigger_probe.rs` | Reverse-engineers the trigger-menu softkey map: presses one softkey on a known page and diffs the settings block. `Fn7` is excluded by construction. Many mode flags (`--type N`, `--page2`, `--knob`, `--walk`, `--alter`, `--verify`, …). | `cargo run -p mso5202d --bin trigger_probe -- --type 2` |
| `capture_corpus.rs` | Records the **triggered** decoder corpus into `scope_dump/decoder_corpus_triggered`: drives the ESP32 generator (`scripts/mso5202d_espgen.py`) into triggered mode, arms a single sequence, sends exactly N PRBS bytes, saves each channel and writes a manifest with the expected bytes. Flags: `--proto`, `--freq`, `--depth`, `--spc`, `--all`. | `cargo run --release -p mso5202d --bin capture_corpus -- --all` |

## Tests (`backend/tests/`)

Rust tests live here as integration tests against the public API — never as inline `#[cfg(test)]`
modules in `src/`.

| File | Hardware | What it covers |
|---|---|---|
| `protocol.rs` | no | `build`/`verify` round-trip, bad checksum, bad length |
| `settings.rs` | no | Param table sums to `SETTINGS_LEN`, unique names, `SIGNED_FIELDS` exist, LE and sign-extension decoding, scaling tables, `TrigState`/`StoreDepth` codes, trigger level scaling |
| `waveform.rs` | no | Analog and logic CSV parsing, `dt_s` from the data, headerless bodies, malformed rows, empty files |
| `device.rs` | no | `Key` ids, knob mappings, timebase direction semantics, `parse_ls`, the shell guard, RGB565 decoding |
| `control.rs` | no | Op labels, `format_volts`/`format_time`, `ProgressEvent` shape and rendering, `csv::selected_source` / `save_in_progress`, WaveData filename handling |
| `decoder.rs` | no | Synthesised UART/SPI/I²C signals with exactly known bytes, plus the shared front-end helpers |
| `parse_real_export.rs` | no | Real front-panel exports from `scope_dump/decoder_corpus` parse from disk |
| `decoder_corpus.rs` | no, `#[ignore]` | Scores the free-running corpus by ramp ratio and **rewrites** `tests/decoder_scores.json`; `git diff` on that file is the regression report. Must run `--release` |
| `decoder_triggered.rs` | no, `#[ignore]` | Grades the triggered corpus byte-for-byte against the manifest's expected bytes; writes `tests/decoder_scores_triggered.json`. Must run `--release` |
| `hardware.rs` | **yes**, `#[ignore]` | One end-to-end device-layer test (the USB interface is exclusive): settings, keys, knobs, volts/div, timebase direction, trigger level, position push, screen, files, shell. It moves the front panel and reverses what it changes |
| `card_shell_roundtrip.rs` | **yes**, `#[ignore]` | A multi-megabyte `0x53` download followed immediately by shell listings, three rounds |
| `link_recovers_from_stale_frames.rs` | **yes**, `#[ignore]` | An abandoned framebuffer grab must not break the next settings read |

Commands, from the root `package.json`:

```sh
pnpm backend:check     # cargo test -p mso5202d && cargo clippy -p mso5202d
pnpm backend:hwtest    # cargo test -p mso5202d --test hardware -- --ignored --nocapture
pnpm decoder:score     # cargo test -p mso5202d --test decoder_corpus --release -- --ignored --nocapture
```

The other ignored tests are run by name, e.g.:

```sh
cargo test -p mso5202d --test decoder_triggered --release -- --ignored --nocapture
cargo test -p mso5202d --test card_shell_roundtrip -- --ignored --nocapture
```

## Conventions for contributors

- **The settings block is read-only.** Nothing in this crate writes it, and no write path is
  exposed — see the "Configuration policy" rustdoc in `backend/src/lib.rs` and the module docs of
  `backend/src/settings/mod.rs`. Configure the scope through key events (`Device::press`,
  `Device::key_edge`, `Device::turn`) and verify against a read-back. A raw block write skips the
  firmware side effects a real key press runs — LEDs, on-screen state, acquisition reconfiguration,
  SD-card detection — and breaks the save path.
- **Tests go in `backend/tests/`.** No inline `#[cfg(test)]` modules in source files.
- **Respect the layering.** Layer 0 stays pure; `device` performs single operations and holds no
  policy; anything multi-step, verified or menu-aware belongs in `control`.
- **Never press `Fn7`.** On this instrument it opens the dual-window / logic-analyzer view, toggles
  `LA-SWI` and jumps to menu 61, stranding any navigation in progress. Use `Fn0`–`Fn6`.
- **Never shell out to `rm`.** Deleting exported CSVs goes through the front-panel delete softkey in
  `control::clear_card`; `device::shell::check_command` blocks the destructive programs outright.
- **Softkeys have no fixed meaning.** Confirm the menu with `converge::open_menu` (or an equivalent
  read of `CONTROL-MENUID`) before pressing one.
- **Assume presses can be dropped.** The scope's key mailbox holds one slot. A value that did not
  move is more often a dropped press than an end stop; follow the retry-then-conclude pattern the
  `converge` helpers already implement.
- **Document new instrument facts in `docs/MSO5202D-protocol.md`**, not here — this file describes
  the code, that one describes the device.
