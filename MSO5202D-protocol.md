# Hantek MSO5202D — USB protocol (reverse-engineered from captures)

Decoded from `mso5202d-session1.pcapng` / `session2.pcapng` (Wireshark/USBPcap
of the vendor "Scope 2.0.0.6" app driving the scope). This is the actual wire
protocol on the bulk endpoints — **not** the Hantek DSO-2xxx/52xx bulk protocol
OpenHantek implements. It is a self-describing, text+binary, `'S'`-framed
protocol.

## 1. Transport
- USB `049f:505a`, vendor-specific class. One interface, two bulk endpoints:
  - **OUT `0x02`** — host → scope commands.
  - **IN  `0x81`** — scope → host responses / data.
- Every logical command is one bulk OUT write, followed by one or more bulk IN
  reads for the response. The app also sends frequent zero-length OUT packets
  (URB bookkeeping; ignore).

### Linux connection recipe (CONFIRMED working, 2026-07)
The scope was designed for a Windows vendor driver (ezusb/`dstusb.sys`) that owns
the endpoints from enumeration and keeps an IN transfer permanently posted. To
talk to it via libusb/pyusb on Linux you must:
1. **Detach the `cdc_subset` kernel driver** from interface 0 (Linux auto-binds it
   by the `049f:505a` ID; it is not really a network device).
2. **`libusb_reset_device()`** (pyusb `dev.reset()`), then re-detach `cdc_subset`
   if it re-binds. *This step is required* — without it the device NAKs all IN
   reads (write succeeds, read times out) because of the stale cdc_subset session.
3. Claim interface 0, `clear_halt` both endpoints.
4. For each request, **post the bulk IN read BEFORE writing the OUT command**
   (e.g. submit the IN transfer from another thread, sleep ~30 ms, then write).
   The device only delivers its reply when an IN transfer is already pending.
See `mso5202d_probe.py` for a working implementation.

## 2. Frame format (both directions)
```
byte 0      : 0x53  ('S')  start-of-frame
byte 1..2   : length, little-endian uint16  ==  (total_frame_len - 3)
byte 3..N-2 : body
byte N-1    : checksum = (sum of all preceding bytes) & 0xFF
```
- **Checksum verified** on many frames, e.g. `53 04 00 12 01 01 6b`:
  0x53+04+00+12+01+01 = 0x6B. And `53 02 00 01 56` → 0x56.
- **Length verified**: settings blob 218 B → bytes[1..2] = `d7 00` (0x00D7=215=218−3);
  protocol.inf 3620 B → `21 0e` (0x0E21=3617); waveform 3847 B → `04 0f` (0x0F04=3844).

> **Note on framing:** byte[1..2] is ONLY the length. The command "selector" is
> the **first payload byte** (byte 3). Earlier drafts mislabeled the length
> low-byte as an opcode — e.g. `53 02 …` and `53 10 …` are length 2 and 16, not
> "cmd 0x02/0x10". Both file reads use the SAME selector 0x10 (read-file); they
> differ only by path.

### Command payload layout (OUT)
`payload = selector(1) | args…`. The response echoes the selector **OR'd with
0x80**. Observed:
| frame bytes | payload | meaning |
|---|---|---|
| `53 02 00 01 56` | `01` | **selector 0x01** keep-alive poll. Variant `53 02 00 00 55` = `00` (stop/start toggle) |
| `53 04 00 12 01 01 6b` / `…00 6a` | `12 01 01` / `12 01 00` | **selector 0x12** SET: `param 0x12, vlen 1, value 0/1` (channel/read select in acquire loop) |
| `53 04 00 02 01 00 5a` | `02 01 00` | **selector 0x02** SET: `param 0x02, vlen 1, value 0` → latch an acquisition |
| `53 10 00 10 00 "/protocol.inf" 66` | `10 00 "/protocol.inf"` | **selector 0x10** READ FILE (`10`, subtype `00`, ASCII path) |
| `53 13 00 10 00 "/keyprotocol.inf" 66` | `10 00 "/keyprotocol.inf"` | selector 0x10 READ FILE (note frame len byte = 0x13 = 19, NOT a different cmd) |

So SET is `selector | vlen | value…` where the selector doubles as the param id;
READ FILE is `0x10 | 0x00 | path`.

### Response payload layout (IN)
```
payload = selectorEcho(1) | subtype(1) | data…
```
- `selectorEcho` = request selector **OR'd with 0x80** (0x02→0x82, 0x12→0x92).
- Examples:
  - `53 07 00 82 00 00 00 0f 00 eb` — response to param 0x02: carries a size
    field `00 0f 00` → **0x0F00 = 3840** = pending waveform byte count.
  - `53 04 0f 82 01 00 <3840 bytes>` — the waveform block (see §5).
  - `53 d7 00 81 01 09 <213 bytes>` — the settings-state blob (see §4).

## 3. File reads — the scope self-describes (selector 0x10)
The scope runs embedded Linux and serves files over USB. The app reads two at
startup:

### `/protocol.inf` (response ~3.6 KB, ASCII INI)
An ordered list of **every setting parameter** the scope exposes, e.g.:
```
[TOTAL]  213
[START]
[VERT-CH1-DISP]  1
[VERT-CH1-VB]    1
[VERT-CH1-COUP]  1
[VERT-CH1-20MHZ] 1
[VERT-CH1-FINE]  1
[VERT-CH1-PROBE] 1
[VERT-CH1-POS]   2
[VERT-CH2-...]   ...
[TRIG-STATE] [TRIG-TYPE] [TRIG-SRC] [TRIG-MODE] [TRIG-COUP] [TRIG-VPOS] ...
[HORIZ-TB] [HORIZ-WIN-TB] [HORIZ-TRIGTIME] ...
[ACQURIE-MODE] [ACQURIE-AVG-CNT] [ACQURIE-TYPE] [ACQURIE-STORE-DEPTH] ...
[MEASURE-ITEM1..8-SRC] ...
[LA-SWI] [LA-CHANNEL-STATE] [LA-...-THRESHOLD-...] ...
[END]
```
The number after each key is the **field width in bytes** of that parameter in
the binary settings blob (§4). `[TOTAL] 213` = number of parameter bytes
(matches the 215-byte blob payload minus framing).

### `/keyprotocol.inf` (response ~0.9 KB, ASCII INI)
Lists the front-panel keys (`[FN-0-KEY]`, `[MENU-MEASURE-KEY]`, `[CT-AUTOSET-KEY]`,
`[VT-CH1-MENU-KEY]`, …). Used for the app's virtual-panel buttons.

## 4. Settings-state blob (IN, 218 bytes, `53 d7 00 81 01 …`)
The app polls this continuously to mirror the scope's live state. Payload is a
**binary struct whose fields are the `/protocol.inf` list in order** (widths from
that file). By diffing 10 distinct snapshots while volts/div and timebase were
changed on the front panel, these payload offsets change:

| offset(s) | width | observed | interpretation |
|---|---|---|---|
| 5 | 1 | 09,0a,00,08 | acquisition / trigger status counter |
| 24 | 1 | 02,03 | a 2-state field (trig mode/coupling) |
| 29–30 | 2 LE (signed) | 33, 3, −7, 83 | trigger level / position |
| 31–33 | 3 LE | **1,000,000** ↔ 1,203,000 ↔ 0 | **timebase** (per-div, ~ps/ns units) |
| 159, 160 | 1+1 | 0x10→0x11→0x12 | **volts/div index**, CH1 & CH2 |
| 217 | 1 | — | frame checksum |

**VERIFIED on hardware** via `mso5202d_probe.py`: a live poll decoded
`timebase@31 = 1,000,000`, `vdiv_ch1 = vdiv_ch2 = 15 (0x0F)`, `status@5 = 9`,
matching the front-panel state and the capture baseline. The offset map is
correct.

(Exact offsets are within the framed payload starting after `53 d7 00 81 01`.)

## 5. Waveform readout
Acquire loop, per refresh:
```
OUT 53 04 00 12 01 00      ; select (channel 0)     -> IN 53 04 00 92 01 00     (ack)
OUT 53 04 00 02 01 00      ; latch/acquire          -> IN 53 07 00 82 00 00 00 0f 00   (size=0x0F00=3840)
                                                     -> IN 53 04 0f 82 01 00 <3840 samples>  (data)
                                                     -> IN 53 04 00 82 02 00   (end marker, subtype 02)
```
- **Samples are 8-bit unsigned, one byte each**, 3840 per block (store depth
  dependent). In the cal-signal capture the bytes cluster at two levels
  (~`0x2E` low, ~`0xED` high) = a clean square wave.
- Param 0x12 toggling 0/1 was assumed to be channel select, but **on hardware
  selecting ch0 vs ch1 returns byte-for-byte identical data** — so 0x12 does NOT
  switch the readout channel (or the block is single-channel / needs de-interleave).
  Real 2-channel readout is still an OPEN QUESTION (see §6).
- Vertical scaling (counts → volts) uses the volts/div index from §4 plus a
  calibration table (not yet captured; likely in another scope file such as a
  calibration `.inf`).

### How the app shows the correct V/div and time/div
The displayed units are **read from the scope, not computed from the samples.**
The waveform stream is raw 8-bit ADC counts with no embedded scale. The app
polls the settings-state blob (§4) and reads the volts/div index (offsets
159/160) and timebase value (offsets 31–33), which change the instant a
front-panel knob is turned; it then maps those to real units via a lookup/cal
table. So scale is fully recoverable from the blob — independent of the sample
data.

## 6. Open questions / next captures
1. **Settings WRITE commands** (set volts/div, timebase, trigger from the host):
   both sessions only changed settings on the front panel, so host→scope writes
   for those params weren't captured. The mechanism is known (cmd 0x04 with the
   param id + value), but the exact **param ids and value encodings** for
   volts/div / timebase need a capture where those are changed **in the app**
   (if the app supports control — it may be a passive viewer).
2. **Calibration / units**: how the volts/div index and timebase value map to
   real V and s. Probably another `/*.inf` file readable via cmd 0x10 — worth
   dumping `/`-listing or known names (e.g. `/system.inf`, `/cal.inf`).
3. **LA (logic-analyzer) channels**: `[LA-*]` params exist; not exercised.

## 7. Implication for OpenHantek
This protocol shares nothing with OpenHantek's `Bulk*`/`Control*` command codes.
Supporting the MSO5202D means writing a **new protocol backend** (frame builder
+ checksum, file reads, settings-blob parser, 8-bit waveform assembler), reusing
only OpenHantek's libusb plumbing — plus a per-model IN endpoint `0x81` and a
`cdc_subset` kernel-driver detach on Linux. A standalone prototype (see
`mso5202d_probe.py`) is the sensible first step before any OpenHantek wiring.
