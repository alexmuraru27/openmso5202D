# Hantek MSO5202D — USB protocol reference

Complete, byte-level reference for the Hantek MSO5202D oscilloscope (USB `049f:505a`),
reverse-engineered from USB captures of the vendor application and confirmed by live
hardware testing. It reads top-down and lets a driver be rebuilt from scratch: every
command, every reply byte, and every settings field is documented, each tagged
**[verified]** (seen on the wire / confirmed on hardware), **[inferred]** (cross-referenced
from captures or the vendor manual), or **[gap]** (still unknown).

## 0. Overview & document status

This is a complete, byte-level reference for the USB protocol of the **Hantek
MSO5202D** oscilloscope (USB ID `049f:505a`). It is written so that a reader
with no prior context can re-implement a full host-side driver from scratch and
account for **every byte** on the wire.

### What the device is

- **Hantek MSO5202D** — a 2-channel, **200 MHz** benchtop mixed-signal
  oscilloscope (2 analog channels + 16 logic-analyzer channels). Unit tested
  here reports software `3.2.35(180502.0)` and identifies itself internally as
  model `dst1202b` with a `[bw]200` (200 MHz) bandwidth tag. [verified]
- Internally it is a small **embedded Linux** computer on a **Samsung S3C-family
  ARM** SoC (part of the widely-cloned Hantek/Tekway/Voltcraft "DSO" hardware
  family). The scope's own firmware renders the screen, runs the acquisition
  engine, and serves this USB protocol; the host is a thin client. [verified]
- The USB protocol is **self-describing**: the scope serves ASCII `.inf` files
  over the same link that enumerate every setting parameter and every
  front-panel key by name (see §4 in the selector reference and the appendices).
  Because of this, a driver never has to hard-code the settings layout — it can
  read it from the instrument.
- This protocol is **unrelated** to the Hantek DSO-2xxx/52xx FX2-based USB-scope
  protocol. It is a custom `'S'`-framed protocol carrying INI files, a fixed
  binary settings blob, and 8-bit waveform records. Do not use FX2/Cypress
  scope references for it.

### External reference & credit

The community article **"Hantek_Protokoll"** on mikrocontroller.net
(<https://www.mikrocontroller.net/articles/Hantek_Protokoll>) documents the same `'S'`/`'C'`
protocol for the sibling **DSO5xxxB** family. It corroborates our framing and key-code list and
describes the `0x43 0x43` **press/release** key command (release = `keycode | 0x80`), documented and
**credited to that article** in **§5.8** and **§9.3.1**. On the MSO5202D a key event is a single
`0x13 <keyid>` inject per frame (the second byte is a don't-care) that drives every softkey,
including the CSV Source cycle (§9.4).

### The two command spaces (leaders)

Every frame in either direction begins with a **leader byte** that selects one
of two independent command spaces (full framing in §3):

| Leader | ASCII | Channel | Role |
|---|---|---|---|
| **`0x53`** | `'S'` | **Data channel** | The normal client protocol: connect, poll settings, acquire waveforms, read files, write settings, key events, screenshot. This is what the vendor app and our driver use. [verified] |
| **`0x43`** | `'C'` | **Command / service channel** | A private service/debug channel: run a shell command on the scope's Linux, read/write raw FPGA regions, beep, commit settings. **The vendor app never uses it** (zero `0x43` frames in any capture); treat it as an advanced/optional surface. [verified] |

The two leaders have **separate selector maps** — the same selector byte means
different things under `0x53` vs `0x43`. This document specifies the `0x53`
(data) space as the primary API and the `0x43` (command) space separately.

### Evidence legend

Every non-trivial claim below is tagged:

- **[verified]** — seen directly on the wire in a capture, and/or confirmed by
  live experiment on the real instrument (hardware sessions **2026-07-08 through
  2026-07-10**). Example hex frames given for a claim are real bytes.
- **[inferred]** — not directly exercised, but concluded from cross-referenced
  captures, the returned self-description files, or the vendor manual (the manual
  covers the 60/100 MHz MSO5000B base model; our MSO5202D is the 200 MHz variant
  and extends the fast timebase one detent further, so treat manual-only numbers
  as inferred).
- **[gap]** — not yet known; flagged so a re-implementer knows what is unproven.

### Provenance of the evidence

- **`scope_dump/captures_wireshark/*.pcapng`** — Wireshark/USBPcap and Linux `usbmon` captures of USB
  traffic, filtered to the scope's device only. Two kinds: (a) the **vendor
  Windows app** ("Scope 2.0.0.6") driving the scope — the ground truth for how a
  correct client behaves; and (b) our own Linux/pyusb driver driving the scope
  while a single control (a knob, a menu, a button) was swept, used to map each
  settings field to a byte offset. File names are cited inline next to the claim
  they support (e.g. `mso5202d-ch1-vdiv.pcapng` mapped the V/div field).
- **Live hardware testing (2026-07-08 … 2026-07-10)** — direct experiments with
  the real scope over pyusb: connecting, polling, acquiring, reading files,
  sending key events, grabbing the framebuffer, and decoding saved CSVs. Facts
  established this way are tagged [verified] with the observed bytes.

Where this document states an exact internal buffer size, checksum rule, or
reply layout, that value was **observed on the wire** and, where noted,
reproduced live on hardware.

---

## 1. USB transport

The scope is a single-configuration, single-interface USB **high-speed bulk**
device. There is no vendor-request control traffic in normal operation — all
protocol data flows over one bulk OUT / one bulk IN endpoint.

| Property | Value | Notes |
|---|---|---|
| **VID:PID** | **`049f:505a`** | Linux mislabels it "CDC Subset Device / Itsy" — misleading (see below). [verified] |
| Device class | **`255`** (Vendor Specific) | Not actually a CDC/network device despite the label. [verified] |
| Interfaces | **1** interface, class 255, 2 endpoints | No alternate settings needed. [verified] |
| Bulk **OUT** | endpoint **`0x02`** | host → scope: every command frame. [verified] |
| Bulk **IN** | endpoint **`0x81`** | scope → host: every reply/data frame. [verified] |
| Max packet | **512 bytes** | USB high-speed bulk. Replies larger than one packet span multiple 512-byte packets — see §2 rule 5 and §3. [verified] |
| Firmware upload | none | The scope boots its own embedded Linux; the host does not upload firmware. [verified] |

The device presents itself internally as a USB gadget-serial endpoint pair; from
the host side it is simply "write a framed command to `0x02`, read the framed
reply from `0x81`."

### Windows vs Linux driver binding

- **Windows:** the vendor driver `dstusb.sys` (a Cypress **EZ-USB** derivative —
  it creates a device the app opens as `\\.\Ezusb-0`) owns the interface from
  enumeration and drives it with EZ-USB bulk read/write IOCTLs. Crucially, that
  driver keeps an **IN transfer permanently posted**, which is the behavior the
  scope's gadget expects (see §2 rule 4). [verified]
- **Linux:** the generic **`cdc_subset`** usbnet driver auto-binds the device
  (creating a fake `usb0` network interface) purely because `049f:505a` happens
  to be that driver's built-in default ID. **The device is not a network
  device** — this binding is an accident of the ID and must be undone before the
  protocol can be spoken (§2). [verified]

---

## 2. Linux connection recipe

The scope was designed around the Windows EZ-USB driver, which owns the
endpoints from enumeration and always has an IN transfer in flight. Reproducing
that from libusb/pyusb on Linux requires the following **exact** sequence. Each
step is load-bearing; the two marked *critical* were established empirically
(identical code fails without them).

1. **Detach the `cdc_subset` kernel driver from interface 0.**
   On Linux `cdc_subset` claims interface 0 the instant the scope is plugged in.
   Until it is detached, `claim_interface` fails with `LIBUSB_ERROR_BUSY` and no
   bulk I/O is possible. [verified]

2. **Reset the device (`libusb_reset_device` / `dev.reset()`), then re-detach
   `cdc_subset`** if it re-binds after the reset. *(critical)*
   Without the port reset the OUT write succeeds but **every IN read times out**:
   `cdc_subset` left the device-side gadget in a usbnet session state in which it
   will not answer the scope protocol. The reset re-initializes the gadget to a
   clean state. Because a reset re-enumerates the device, `cdc_subset` may grab
   it again — hence the second detach. [verified]

3. **Claim interface 0, then `clear_halt` on both endpoints** (`0x02` and
   `0x81`). This resets the bulk data toggles to DATA0 on both host and device so
   the first transfers aren't silently dropped by a toggle mismatch left over
   from `cdc_subset`. [verified]

4. **Post the bulk IN read BEFORE writing the OUT command.** *(critical — the
   single most important transport quirk)*
   The device delivers its reply **only if an IN transfer is already pending**
   when the command arrives (mirroring the Windows driver, which always has an IN
   posted). With naive synchronous *write-then-read*, the reply is missed and the
   read times out. The working pattern: run the IN read on a background thread,
   sleep ~30 ms so the IN request is actually in flight, then issue the OUT
   write. [verified]

5. **Keep a persistent RX buffer and consume whole frames.**
   Replies span multiple 512-byte USB packets, and several logical frames often
   arrive back-to-back (e.g. a file read returns one or more *content* frames
   **and** an *end-marker* frame — see §3 and the selector reference). If
   leftover bytes are discarded instead of retained, the next read starts
   mid-frame and the stream **desyncs**. Symptoms seen when this was wrong: an
   "empty" file read, and a settings blob that decoded as stray INI text. Keep a
   rolling buffer, pull out complete frames by their length field, and leave the
   remainder for the next read. Provide a **resync** path (drop bytes until a
   valid leader + length + checksum is found) to recover after a timeout or a
   corrupt frame. [verified]

**Running without root:** install a udev rule granting access to `049f:505a`
(repo file `70-mso5202d.rules` → `/etc/udev/rules.d/`, reload, replug).
Otherwise run as root. Note a GUI (matplotlib) under `sudo` loses X access, so
the udev rule is the correct approach for the live viewer. [verified]

### A mid-session USB drop + reconnect is NORMAL

The scope application runs under a **supervisor** on the instrument that watches
the app over a local heartbeat (~100 ms). If the app restarts (or is restarted
by the supervisor), the USB gadget briefly disappears and **re-enumerates within
~100 ms**. A host driver should therefore treat a sudden bulk error / device
drop as **transient**: tear down, re-run the connection recipe (steps 1–3), and
resume — do not assume the scope is gone. This respawn-and-reconnect is expected
behavior, not a fault. [verified]

---

## 3. Framing & checksum

Both directions and both leaders use one framing scheme:

```
byte 0        : leader   0x53 ('S', data channel)  |  0x43 ('C', command channel)
byte 1..2     : length   little-endian uint16   ==  (total_frame_len - 3)
byte 3        : selector (= payload[0])
byte 3..N-2   : payload  (selector + arguments / data)
byte N-1      : checksum = (sum of all preceding bytes) & 0xFF
```

### The length field

`length = bytes[1..2]` little-endian **= total_frame_len − 3** — i.e. it counts
the payload plus the one checksum byte, but **not** the leader or the two length
bytes themselves. Equivalently, `total_frame_len = length + 3`. [verified]

Worked examples (all real frames):

| Frame purpose | Length bytes | Decoded length | Total frame |
|---|---|---|---|
| Poll settings (OUT) | `02 00` | 2 | 5 bytes |
| Settings blob (IN, 213 params) | `d7 00` | 0x00D7 = 215 | 218 bytes |
| `/protocol.inf` file (IN, 3617 payload) | `21 0e` | 0x0E21 = 3617 | 3620 bytes |
| Waveform data frame (IN, 3840 samples) | `04 0f` | 0x0F04 = 3844 | 3847 bytes |

> **Framing gotcha:** bytes[1..2] are the *length only*. The **selector** is the
> first payload byte (byte 3). Do not mistake the length low-byte for an opcode
> — `53 02 …` and `53 10 …` are lengths 2 and 16, not "command 0x02 / 0x10".

**Rule: `length == 0` is invalid.** A frame whose length field decodes to zero
is rejected by the device's frame validator. Every real frame has at least the
1-byte selector payload (minimum length 2). [verified]

### The checksum — and the `0x66` wildcard

The trailing byte is `checksum = (sum of every byte before it) & 0xFF`, i.e. the
8-bit sum of `bytes[0 .. N-2]` (leader, both length bytes, and the whole
payload). [verified]

Verification examples:
- `53 04 00 12 01 01 6b` → 0x53+0x04+0x00+0x12+0x01+0x01 = **0x6B** ✓
- `53 02 00 01 56` → 0x53+0x02+0x00+0x01 = **0x56** ✓

**Wildcard: a checksum byte of `0x66` is accepted on ANY frame, unchecked.**
The device's validator accepts a frame if the computed checksum matches **OR**
the checksum byte equals **`0x66`**. So a client may send `0x66` in place of a
correctly computed checksum on *any* command and it will be honored. [verified]

- Proven live: a settings poll sent as `53 02 00 01 66` (wrong checksum, `0x66`
  wildcard) returned the full settings blob. [verified]
- The file-read command in fact **always** uses the `0x66` wildcard in the
  vendor app's traffic (e.g. `53 10 00 10 00 2f70726f746f636f6c2e696e66 66` =
  read `/protocol.inf`); every other selector uses a computed checksum. Both
  forms are accepted for file reads. [verified]

### Selector and the reply echo

- **`selector = payload[0]`** (frame byte 3). It names the command within the
  leader's command space.
- **The reply's echo byte is ALWAYS `(selector & 0x7f) | 0x80`.** The device
  builds every reply by copying the request's leader + length + selector, then
  forcing the selector's high bit set (and clearing any incoming high bit first).
  So `0x01`→`0x81`, `0x02`→`0x82`, `0x10`→`0x90`, `0x11`→`0x91`, `0x12`→`0x92`,
  `0x13`→`0x93`, `0x20`→`0xa0`, `0x21`→`0xa1`; and on the `0x43` leader
  `0x7f`→`0xff`. This echo rule is uniform — there is no per-command exception.
  [verified]

The reply's own length and checksum are recomputed by the device for the actual
reply size, following the same length/checksum rules above.

### Two selector spaces keyed by the leader

The leader byte both frames the packet and selects which of two command maps the
selector is looked up in:

- **`0x53`** — the data-channel selector map (valid selectors `0x00`–`0x21`;
  many values in that range are no-ops that produce no reply). Documented as the
  primary API.
- **`0x43`** — the command-channel selector map (a larger map up to `0x7f`).
  Documented separately as the service/advanced surface.

A selector value that is not implemented in the relevant map produces **no
reply** (the device silently ignores it). A client must therefore only send
known selectors, and must not block forever waiting on a reply that will never
come. [verified]

### IN-echo table (reply's echo byte per command)

The following echo bytes were all validated on the wire. "Reply shape" is
detailed per-selector in the selector-reference sections; it is summarized here
so the framing is complete.

| Leader | Selector (OUT) | Echo (IN) | Reply shape (summary) |
|---|---|---|---|
| `0x53` | `0x00` connect/ping | `0x80` | empty ack |
| `0x53` | `0x01` poll settings | `0x81` | 213-byte settings blob (no subtype byte) |
| `0x53` | `0x02` acquire | `0x82` | size(`00`) / data(`01`) / end(`02`) / no-data(`03`) frames |
| `0x53` | `0x10` read file | `0x90` | content(`01`) frames … + end(`02`, carries an 8-bit file sum) |
| `0x53` | `0x11` write settings | `0x91` | 1 status byte (`00` ok / `FF` fail) |
| `0x53` | `0x12` acq/run latch | `0x92` | `92 00` (sub 0) or `92 01 <latch>` (sub 1) |
| `0x53` | `0x13` key event | `0x93` | 1 status byte (live menu/key status) |
| `0x53` | `0x14` descriptor write | (none) | no reply (pairs with `0x21`) |
| `0x53` | `0x20` framebuffer | `0xa0` | content(`01`) frames = 768000 B … + end(`02`, carries an 8-bit image sum); **no size frame** |
| `0x53` | `0x21` descriptor read | `0xa1` | id16 + a few descriptor bytes |
| `0x43` | `0x00` FPGA reg read | `0x80` | registers as LE32 words |
| `0x43` | `0x01` engine sample read | `0x81` | debug sample block |
| `0x43` | `0x02` region-1 dump | `0x82` | 5072-byte block |
| `0x43` | `0x03` region-2 dump | `0x83` | 8664-byte block |
| `0x43` | `0x10` read file | `0x90` | file content/end (same as `0x53`/`0x10`) |
| `0x43` | `0x11` shell exec | `0x91` | command stdout as content/end frames |
| `0x43` | `0x40` / `0x41` region write | `0xc0` / `0xc1` | write ack |
| `0x43` | `0x42`/`0x43`/`0x44`/`0x45` | `0xc2`/`0xc3`/`0xc4`/`0xc5` | empty ack |
| `0x43` | `0x50` / `0x60` | `0xd0` / `0xe0` | param command [gap] |
| `0x43` | `0x7f` commit settings | `0xff` | empty ack |

For `0x53`/`0x10` (file), `0x53`/`0x02` (acquire) and `0x53`/`0x20`
(framebuffer), the byte immediately after the echo is a **subtype** byte
(`00`=size, `01`=content/data, `02`=end, `03`=no-data) that distinguishes the
frames of a multi-frame reply. The **settings poll (`0x81`) has no subtype
byte** — its 213 parameter bytes begin immediately after the echo. These
per-command details are specified in the selector-reference sections. [verified]

## 4. Command reference — leader `0x53` (data channel)

The `0x53` leader (`'S'`) is the **data channel** — the only channel the vendor
Windows app uses. Everything a driver needs (settings, waveforms, screenshots,
key events, file transfer) rides on it. This section is one subsection per
selector, in ascending order, each with the exact OUT byte layout, the exact
reply frame(s), a concrete example hex frame, and a verified/inferred/gap tag on
every non-trivial claim.

**Frame template (recap of §3).** Every OUT frame is

```
53 | len_LE16 | selector | args… | ck
```

where `len_LE16 = framelen − 3` (payload + checksum, i.e. every byte after the
3-byte `53 | len` header), and `ck = (sum of all bytes before it) & 0xFF`. A
checksum byte of **`0x66` is a wildcard** the device accepts on any frame in
place of the computed value. Every reply **echoes the selector with bit 7 set**:
reply byte 3 = `selector | 0x80`. This is uniform — there is no per-command
echo. [verified]

Throughout, `iN` denotes OUT-frame byte `N` (so `i3` = selector, `i4` = first
argument).

### Selector map (leader `0x53`)

| Sel | Name | Echo | OUT form | Reply shape |
|---|---|---|---|---|
| `0x00` | Ping / Connect | `0x80` | `53 02 00 00` | empty ack |
| `0x01` | Poll settings | `0x81` | `53 02 00 01` | 213 param bytes (no subtype) |
| `0x02` | Acquire waveform | `0x82` | `53 04 00 02 01 <ch>` | size(`00`)/data(`01`)/end(`02`), or no-data(`03`) |
| `0x10` | Read file | `0x90` | `53 <len> 10 00 <path>` | content(`01`)×N + end(`02`, +sum8) |
| `0x11` | Write settings | `0x91` | `53 D7 00 11 <213 B>` | status `00`=ok / `FF`=fail |
| `0x12` | Latch (run / acq-mode) | `0x92` | `53 04 00 12 <sub> <val>` | `92 <sub> [val]` |
| `0x13` | Key event | `0x93` | `53 04 00 13 <keyid> <state>` | status byte |
| `0x14` | Descriptor write | *(none)* | `53 <len> 14 <id16> <bytes>` | no reply |
| `0x20` | Screen framebuffer | `0xA0` | `53 02 00 20` | content(`01`)×76 + end(`02`, +sum8) |
| `0x21` | Descriptor read | `0xA1` | `53 02 00 21` | `id16 + bytes` |

Selectors **`0x03`–`0x0F` and `0x15`–`0x1F`** pass the frame validator but the
device sends **no reply** — they are reserved/no-op. A read waiting on them will
simply time out. [verified] (no reply observed for any of them)

---

### `0x00` — Ping / Connect

Empty handshake. Used to confirm the link is alive after the connection recipe;
carries no arguments and changes no state.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `0002` LE | len = framelen − 3 |
| 3 | 1 | `00` | selector (ping) |
| 4 | 1 | `55` | checksum |

**Reply** — a single empty ack, echo `0x80`:

```
53 02 00 80 <ck>
```

**Echo:** `0x80`.

**Example (verified on the wire):**

```
OUT  53 02 00 00 55        (0x53+0x02+0x00+0x00 = 0x55)
IN   53 02 00 80 d5        (0x53+0x02+0x00+0x80 = 0xd5)
```

[verified] — seen in `scope_dump/captures_wireshark/control/`. Note `53 02 00 00 55` is selector
`0x00`, a distinct command from `0x01`; it is **not** a "variant of poll". [verified]

---

### `0x01` — Poll settings

Reads the entire device state as one flat **213-byte parameter block** (the
"settings blob"). This is the workhorse for reading everything the front panel
shows; the field-by-field decode of the 213 bytes is the datasheet in §8.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `0002` LE | len |
| 3 | 1 | `01` | selector (poll) |
| 4 | 1 | `56` | checksum |

**Reply** — echo `0x81`, followed immediately by the parameter bytes. **There is
NO subtype byte** — parameter byte 0 (`VERT-CH1-DISP`) sits right after the echo:

```
53 <count+2 LE16> 81 <count parameter bytes> <ck>
```

The length field is `count + 2` (echo byte + checksum). For the current firmware
`count = 213`, so the length field is `215 = 0x00D7`. Total frame = 218 bytes.
The `count` value equals `[TOTAL]` from `/protocol.inf` (§Appendix A); a driver
should trust the frame length rather than hard-code 213, in case a firmware
revision changes the parameter list.

**Echo:** `0x81`.

**Example (verified):**

```
OUT  53 02 00 01 56
IN   53 d7 00 81 01 0a 00 …(213 bytes)… <ck>
              └ param[0]=VERT-CH1-DISP=0x01, param[1..]=…
```

[verified] — the 213-byte length and no-subtype layout are seen in every
`01`→`81` exchange across the capture corpus.

---

### `0x02` — Acquire waveform

Pulls one on-screen acquisition record for a single channel. This is the most
structured command: a **3-frame reply** (size → data → end), plus a 4th "no
fresh data" variant. The sample **byte encoding** (signed int8, rails, trigger
column) is specified in full in §6 — this section covers the request/response
framing and the channel map.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `0004` LE | len |
| 3 | 1 | `02` | selector (acquire) |
| 4 | 1 | `<sub>` | **sub-command**: `01` = acquire, `00` = latch |
| 5 | 1 | `<ch>` | channel code (when `sub=01`) |
| 6 | 1 | `<ck>` | checksum |

`i4` (the "sub" byte) is a real dispatch byte, **not** a constant subtype:

- **`sub = 01` → acquire channel `i5`.** This is the only data-producing form;
  the vendor app always sends `02 01 <ch>`.
- **`sub = 00` → latch** (equivalent to `12 01 01`, see `0x12`); it triggers no
  waveform reply. Unused by the vendor app. [verified]

**Channel codes (`i5`)**

| Code | Channel | Notes |
|---|---|---|
| `00` | CH1 | 1 byte/sample, signed int8 |
| `01` | CH2 | 1 byte/sample, signed int8 |
| `02` | Math | 1 byte/sample; needs CH1 or CH2 displayed. **Vendor never issues `02 01 02`** (Math is computed host-side) [verified] |
| `03` | *(unusable)* | half-wired source; do not use |
| `04` | *(unusable)* | dual/all-analog; do not use |
| `05` | Logic Analyzer | **2 bytes/sample** = 16-bit LE word, bit N = D(N) |

Codes **`≥ 06` are invalid** and desync the stream — never send them. [verified]
Codes `03`/`04` return incoherent data and are likewise avoided. [inferred]

**Precondition — the channel must be displayed.** If the requested channel is
turned off on the scope, the acquire produces **no frames at all** (not even a
size frame) and the read times out. This is the mechanism behind "a channel must
be on-screen to read it". [verified]

**Reply — 3 frames, echo `0x82`.** Subtype is at reply byte 4:

| Frame | Subtype | Layout |
|---|---|---|
| SIZE | `00` | `53 07 00 82 00 <src> <c0> <c1> <c2> <ck>` |
| DATA | `01` | `53 <count+4 LE16> 82 01 <ch> <count data bytes> <ck>` |
| END | `02` | `53 04 00 82 02 <ch> <ck>` |
| NO-DATA | `03` | `53 04 00 82 03 <ch> <ck>` — replaces size/data/end when no fresh block is ready |

Details:

- **SIZE frame** carries the record length as a little-endian value in bytes
  `6..8` — a **24-bit usable count** (the byte at offset 9 is occupied by the
  checksum, so the count is capped at 24 bits; screen records are far smaller).
  **`count` is the DATA-payload BYTE count, not always the sample count**:
  analog is 1 B/sample so `count == samples`; **LA is 2 B/sample so
  `count == 2 × samples`**. Read the count from this frame — **never hard-code
  3840**; it changes with the plot width (see below). [verified]

- **The SIZE-frame `<src>` byte is an internal source id, and for LA it differs
  from the requested code:**

  | Requested `ch` | SIZE `<src>` | DATA/END `<ch>` |
  |---|---|---|
  | `00` (CH1) | `00` | `00` |
  | `01` (CH2) | `01` | `01` |
  | `05` (LA) | `03` | `05` |

  [verified] — the LA size frame reports `82 00 03 …` while its data/end frames
  echo the requested `05`.

- **DATA frame** repeats the request's `sub` (`01`) and `ch` bytes, then the
  sample bytes starting at offset 6. Its length field is `count + 4` (echo `82` +
  `01` + `ch` + `count` data + `ck` = `count + 4`).

- **END frame** must be **consumed** by the reader or the stream desyncs; a
  driver's resync routine keys on recovering this marker after a bad/short read. [verified]

- **NO-DATA (subtype `03`)** is emitted instead of the data path when the
  acquisition engine, after a short internal poll (~10 retries over ~100 ms),
  finds no fresh block. This — not a hard hang — is the byte-level reason raising
  the store depth beyond the screen (e.g. 40K) makes `0x02` "return nothing":
  the screen path has no fresh screen block to serve. Treat subtype `03` like an
  empty/aborted read. [verified]

- **Internal 10 000-sample chunking.** A record longer than 10 000 samples would
  fan out into multiple subtype-`01` DATA frames (≤ 10 000 samples each) before
  the END. At screen depth (≤ 3840) it is always a single DATA frame, which is
  why every observed capture is exactly size/data/end. Deep records are never
  served over USB, so this multi-frame path is not exercised in practice. [inferred]

- **Record length is variable.** Observed counts: **3840** (19.2 divisions) when
  no soft-menu panel is open, **3200** (16 divisions) when a right-hand menu
  panel is open — the panel shaves 3.2 divisions (640 samples) off the plot
  width. The timebase does **not** change the count; the plot width does. LA
  follows in bytes: 7680 (menu closed) / 6400 (menu open). [verified]

- **Sample values** are two's-complement **signed int8** at **25 counts/division**:
  screen-centre = `0x00`, top rail = `0x7F` (+127), bottom rail = `0x81` (−127),
  and any sample inside the **trigger column is forced to `0xFF`**. `0x80` (−128)
  never occurs. counts→volts uses **`Vdiv / 25` volts per count**. Full encoding
  and decode formula are in §6. [verified] — this session read CH1 in the
  `+38..+85` range, CH2 in `−41..+5`, **zero `0x80` bytes**, and exactly one
  `0xFF` trigger-column marker on CH2; counts→volts = `Vdiv/25` matches the
  exported-CSV ground truth.

**Echo:** `0x82`.

**Example (CH1 acquire, verified):**

```
OUT   53 04 00 02 01 00 5a          request CH1
IN    53 07 00 82 00 00 00 0f 00 eb   SIZE  src=0  count=0x000f00=3840
IN    53 04 0f 82 01 00 <3840 B> <ck> DATA  len=0x0f04=3844, ch=0
IN    53 04 00 82 02 00 db            END   ch=0
```

**Example (CH2, verified):** `OUT 53 04 00 02 01 01 5b` → SIZE
`53 07 00 82 00 01 00 0f 00 ec` (src=1, 3840), DATA `82 01 01 …`, END
`53 04 00 82 02 01 dc`.

**Channel switch is one-deep pipelined [verified 2026-07-11].** After changing the
channel byte, the **first** `02 01 <ch>` returns the *previously selected* channel's
buffer; the **second** returns `<ch>`. So a naïve `read(CH1); read(CH2)` yields two
byte-identical blocks (both CH1). To read a specific channel, issue the acquire
**twice and keep the second** (verified on a stopped scope: CH1=[253,254,…] vs
CH2=[208,208,…] only after the double-read; a single read of each gave `diff=0`). This
matters for inter-channel work (serial clk+data): `_direct_acquire` in
`mso5202d_plot.py` double-reads each channel. On a *stopped* scope the second read is
also stable/repeatable; a running scope updates the buffer between reads.

**Example (LA acquire this session, verified):**

```
OUT   53 04 00 02 01 05 5f          request LA
IN    53 04 00 82 03 03 df          NO-DATA (subtype 03, ch=3): no fresh LA block
```

[verified] — this session's LA read returned the `03` no-data reply. In earlier
captures the same request answered with a full 3-frame block whose SIZE was
`82 00 03 …` (src=3) and DATA `82 01 05 …` (7680/6400 bytes). The *usability* of
LA-over-`0x02` content remains an open hardware question (captured LA words were
idle-zero). [gap]

---

### `0x10` — Read file

Streams a file from the scope's on-board Linux filesystem back to the host. This
is how a driver reads `/protocol.inf`, `/keyprotocol.inf`, and — the big one —
pulls a deep-capture CSV that the front panel saved (see §10), sidestepping the
lack of a deep-memory USB acquire.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `<len>` LE | len = framelen − 3 |
| 3 | 1 | `10` | selector (read file) |
| 4 | 1 | `00` | fixed |
| 5 … | `len−3` | ASCII | absolute file path (no NUL terminator) |
| last | 1 | `<ck>` | checksum — normally the **`0x66` wildcard** |

The path length is `len − 3` (excludes selector, the `00`, and the checksum). The
path is a plain absolute path, e.g. `/protocol.inf`, `/help.db`,
`/mnt/udisk/WaveData1410.csv`.

**Reply — echo `0x90`, NO size frame:**

| Frame | Subtype | Layout |
|---|---|---|
| CONTENT | `01` | `53 <chunk+3 LE16> 90 01 <up to 10208 file bytes> <ck>` — repeated |
| END | `02` | `53 04 00 90 02 <sum8> <ck>` |

- The per-content-frame payload cap is **10208 bytes** (not 64 KB). A file of
  size `S` arrives as `ceil(S / 10208)` content frames followed by one end frame. [verified]
- The END payload byte is **`sum8` = the 8-bit sum of every byte of the whole
  file** (`Σ file_bytes & 0xFF`) — a transfer integrity check over the complete
  file, not per-chunk. [verified]
- **No size/`90 00` frame** precedes the content — the host reads content frames
  until it sees the `90 02` end marker. [verified]
- **Checksum on the request:** `0x10` is the one selector for which the vendor
  always sends the **`0x66` wildcard** checksum. A correctly computed checksum is
  **also accepted** — both forms work. [verified] (confirmed on hardware this session)
- **A completed transfer can leave a tail on the endpoint.** After a large read the
  bulk IN endpoint may still hold bytes once the `90 02` end marker has been
  consumed. A following command then reads that residue instead of its own reply:
  a second back-to-back `0x10` read returns **0 bytes**, because the stale end
  marker is taken for its first frame (its subtype is `02`, not `01`, so the
  content loop never runs). **Drain the endpoint after any large read** — the same
  precaution the `0x20` framebuffer grab needs. Verified with two 40 K CSV
  read-backs (768 153 B then 761 532 B): without the drain the second returns
  empty; with it, both return byte-exact. [verified 2026-07-20]
- **Not every path is servable.** A path the firmware will not read back answers
  with a **1-byte reply** rather than an error or an empty stream, and no amount of
  waiting changes it: `/dso_bin` (the 4 454 536-byte running acquisition binary)
  returns 1 byte with both a 4 s and a 30 s per-frame timeout. Since the reply
  carries no declared length, a reader cannot distinguish this from a complete
  short file except by the `sum8` end-marker or an independent size (`ls`).
  Regular data files of the same order stream normally — `/help.db` (911 360 B)
  reads back byte-exact at ~865 KB/s. [verified 2026-07-20]

**Echo:** `0x90`.

**Example — read `/protocol.inf` (verified):**

```
OUT   53 10 00 10 00 2f 70 72 6f 74 6f 63 6f 6c 2e 69 6e 66 66
                     └────────── "/protocol.inf" (13 bytes) ─────────┘ └ 0x66 wildcard
IN    53 21 0e 90 01 5b 54 4f 54 41 4c 5d …           CONTENT "[TOTAL]…", single frame
IN    53 04 00 90 02 <sum8> <ck>                       END
```

**Example — read `/help.db` (911 360 bytes, verified this session):** the reply
is **90 content frames** (each ≤ 10208 bytes) followed by
`53 04 00 90 02 61 4a` — the end marker whose payload `0x61` equals the 8-bit
sum of all 911 360 file bytes. [verified]

**Throughput:** large-file reads run at roughly **800 KB/s** over USB; a deep
512K-point CSV (~7.7 MB) is retrievable in about 10 s. [verified this session]

---

### `0x11` — Write settings

The inverse of `0x01`: writes the whole 213-byte settings block back to the
device. Any field in the datasheet (§8) can be set this way with a
**read-modify-write** (poll `0x01`, decode, change fields, re-encode, write `0x11`).

**A `0x11` write sets the field but does NOT run the front-panel key handler's
side-effects** — the channel LED, the on-screen radios (LongMem depth, CSV Source),
the acquisition reconfiguration, and (empirically) SD-card detection. Consequences
[verified]: a `VERT-CHx-DISP` write flips the field without lighting the LED or
serving the channel; a depth write leaves the on-screen LongMem radio stale at 4K
and **reboots a running scope**; and configuring a capture by `0x11` was associated
with Save→CSV writing no file and with reboots. **The robust control path is
front-panel keys** (`0x13`) — every knob has ± key ids (§9.2), so a driver can set
V/div, SEC/DIV, trigger level, channels and depth by key and only ever *read*
settings memory. Treat `0x11` as a field poke whose GUI/hardware side-effects may
not follow, not as a full "set control" primitive. `[verified 2026-07-15]`

**A write makes the scope busy ~3.4 s [verified 2026-07-11].** Reapplying the whole
block reconfigures channels/trigger/depth, and the device does not answer the *next*
read until it finishes: the first `0x01` poll after a write blocks ~3.4 s, while
subsequent polls return in ~0.02 s. So write only when a field actually changed
(compare the re-encoded block to the just-read one and skip a no-op write) — this is
the dominant cost in any capture loop that re-preps each time.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `00D7` LE | len (= 213 + 2) |
| 3 | 1 | `11` | selector (write settings) |
| 4 … | 213 | bytes | the full parameter block (same layout as the `0x01` reply payload) |
| last | 1 | `<ck>` | checksum |

The block starts immediately at `i4` (no subtype byte) and is exactly 213 bytes
today (`datalen = len − 2`). It is walked with the same `/protocol.inf` field
table used to decode `0x01`, writing each named field.

**Reply** — echo `0x91`, one status byte:

```
53 03 00 91 <status> <ck>       status: 00 = success, FF = failure
```

**Echo:** `0x91`.

**Example (verified):**

```
OUT   53 d7 00 11 01 0a 00 …(213 bytes)… <ck>
IN    53 03 00 91 00 e7                       status 00 = OK
```

[verified] — round-tripping a polled block back with `0x11` and re-reading with
`0x01` reproduces the written fields.

---

### `0x12` — Latch (run / acquisition mode)

A two-argument control latch. It is **not** a stop/panel-lock and **not** a
channel selector (both were earlier misconceptions). The vendor app uses only
`sub=1`, pulsing the value `1 → 0` around every refresh as part of its acquire
loop.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `0004` LE | len |
| 3 | 1 | `12` | selector (latch) |
| 4 | 1 | `<sub>` | `01` = run-latch, `00` = acquisition-mode |
| 5 | 1 | `<val>` | boolean value |
| 6 | 1 | `<ck>` | checksum |

- **`sub = 1` (run-latch):** sets the acquisition run latch to `val != 0` and
  kicks the acquisition. **Reply echoes the new latch value:** `53 04 00 92 01
  <val> <ck>`. This is the one the vendor pulses `12 01 01` → `12 01 00` every
  refresh. [verified]
- **`sub = 0` (acquisition-mode):** sets an internal acquire-mode flag from `val`.
  **Reply has no value byte:** `53 03 00 92 00 <ck>`. Vendor never uses it. [verified]

Only `sub ∈ {0, 1}` do anything; other sub values fall through with no reply.

**Echo:** `0x92`.

**Examples (verified this session):**

```
OUT   53 04 00 12 01 01 6b        sub=1 run-latch, val=1
IN    53 04 00 92 01 01 eb        echoes val

OUT   53 04 00 12 00 00 69        sub=0 acq-mode, val=0
IN    53 03 00 92 00 e8           subtype 00, no value byte
```

[verified] — neither combination stops acquisition or locks the panel; the
"sub=0→STOP / sub=1→PANEL-LOCK" description from third-party notes is wrong.

---

### `0x13` — Key event

Injects one front-panel key press. Because whole menus and actions (Autoset,
Run/Stop, Default Setup, Force-Trigger, Set-50%, every soft-key) are reachable as
keys, this plus `0x11` is enough to drive the entire UI remotely.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `0004` LE | len |
| 3 | 1 | `13` | selector (key event) |
| 4 | 1 | `<keyid>` | 0-based index into `/keyprotocol.inf` |
| 5 | 1 | `<state>` | **IGNORED by the device** |
| 6 | 1 | `<ck>` | checksum |

- `keyid` (`i4`) is the 0-based position of the key in `/keyprotocol.inf`
  (§Appendix B). Known ids: Autoset = 17, Run/Stop = 19, Default Setup = 21,
  Trig-50% = 46, Force = 47.
- **The state byte `i5` is ignored** — every `0x13` frame is exactly **one key
  press** regardless of `state`. The vendor always sends `01`, but `00` behaves
  identically. Send one frame per press; do not model press/release. [verified]
- The key is delivered through a **single-slot mailbox** the UI consumes on its
  own poll — issuing keys faster than the UI drains them can drop one. Space key
  events out. [inferred]

**Reply** — echo `0x93`, one status byte:

```
53 03 00 93 <status> <ck>
```

The status byte is the device's **current menu/key status** at reply time (it
changes as menus open/close — observed values `0x01`, `0x0b`, `0x19`). It is
**not** a fixed echo of the request's state byte. [verified]

**Echo:** `0x93`.

**Example (Autoset, verified):**

```
OUT   53 04 00 13 11 01 7c        keyid 0x11=17 (Autoset), state 01 (ignored)
IN    53 03 00 93 01 ea           status byte = current menu status
```

[verified] — key ids 17/19/21/46/47 observed on the wire, all with state `01`.

---

### `0x14` / `0x21` — Descriptor write / read

A matched write/read pair over a small static descriptor structure carrying a
16-bit id and a few bytes. Purpose is **unknown** — likely a version/capability
descriptor. The vendor app never uses either selector; documented for
completeness. [gap]

**`0x14` — descriptor WRITE**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `<len>` LE | len |
| 3 | 1 | `14` | selector |
| 4–5 | 2 | `<id16>` LE | descriptor id |
| 6 … | n | bytes | descriptor field bytes |
| last | 1 | `<ck>` | checksum |

`0x14` **emits no reply** — it silently stores the descriptor. [verified] (no
reply frame ever follows)

**`0x21` — descriptor READ**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `0002` LE | len |
| 3 | 1 | `21` | selector |
| 4 | 1 | `<ck>` | checksum |

**Reply** — echo `0xA1`, the 16-bit id then the stored bytes:

```
53 <len> a1 <id16 LE> <b1> <b2> … <ck>
```

**Echo:** `0xA1`.

**Example (verified):**

```
OUT   53 02 00 21 76
IN    53 09 00 a1 d9 07 01 01 03 01 18 fb
                  └id16 = 0x07D9 = 2009┘ └ 01 01 03 01 18 (payload) ┘
```

The default id is **`0x07D9` = 2009**; the meaning of the id and the trailing
bytes is unresolved. [gap]

---

### `0x20` — Screen framebuffer

Grabs the scope's rendered LCD as a raw RGB565 bitmap. The vendor "virtual
panel" is just this command streamed continuously — it is a real screenshot
(including firmware-drawn LA rows), not a host-side re-render. This is also the
**safe way to view LA** (the firmware draws the D-rows into the screen), avoiding
the unreliable `02 01 05` path.

**OUT layout**

| Offset | Width | Value | Meaning |
|---|---|---|---|
| 0 | 1 | `53` | leader |
| 1–2 | 2 | `0002` LE | len |
| 3 | 1 | `20` | selector (framebuffer) |
| 4 | 1 | `75` | checksum |

**Reply — echo `0xA0`, NO size frame:**

| Frame | Subtype | Layout |
|---|---|---|
| CONTENT | `01` | `53 <chunk+3 LE16> a0 01 <up to 10208 pixel bytes> <ck>` — repeated |
| END | `02` | `53 04 00 a0 02 <sum8> <ck>` |

- Total image = exactly **768000 bytes = 800 × 480 × 2** (16-bpp RGB565). [verified]
- Delivered as **76 content frames**: 75 full frames of 10208 bytes + 1 tail of
  2400 bytes (`75 × 10208 + 2400 = 768000`), then one END. **No size frame.** [verified]
- The END payload byte is **`sum8` = the 8-bit sum of all 768000 pixel bytes**
  (`Σ pixel_bytes & 0xFF`) — a whole-image integrity check. [verified]
- Pixels are little-endian RGB565; the exact channel bit order (assumed
  R:5 G:6 B:5, MSB=R) is not independently confirmed. [gap]

**Echo:** `0xA0`.

**Example (verified):**

```
OUT   53 02 00 20 75
IN    53 e3 27 a0 01 93 31 93 31 … <ck>      CONTENT frame 1 (len 0x27e3 = 10211 = 10208+2+1)
      … 74 more full content frames …
IN    53 63 09 a0 01 <2400 px bytes> <ck>    CONTENT frame 76 (len 0x0963 = 2403)
IN    53 04 00 a0 02 47 40                   END: img sum8 = 0x47
```

[verified] — 76 content frames sum to exactly 768000 pixel bytes; the first
pixels `93 31` decode as the RGB565 word `0x3193`.

## 5. Command reference — leader `0x43` (command / shell / FPGA channel)

Everything in Section 4 used the leader byte `0x53` — the *data channel* the vendor
"Scope" application actually speaks. The device answers a **second, parallel command
set under leader `0x43`** ("C" for *command*). This is a **private service / debug
channel: the vendor app never issues a single `0x43` frame** — across every capture in
`scope_dump/captures_wireshark/` and `scope_dump/captures_wireshark/control/` there are **zero** `0x43`-leader OUT frames (the
lone `43` byte that shows up in a naive scan is the ASCII letter "C" inside file
payload, not a frame leader). [verified]

The channel was **discovered from the sibling open-source project
`github.com/onnokort/dsoc`** (which drives the same 049f:505a silicon family) and every
selector below was then **confirmed on this MSO5202D over its own USB link**. Because no
vendor traffic exercises it, treat it as powerful but unsupported: several selectors
**write** device memory or run **arbitrary root shell commands**, and a few can **brick
the instrument**. The read-only selectors (`0x00`–`0x03`, `0x10`, and — with care —
`0x11`) are safe and genuinely useful (FPGA register introspection, region dumps, file
pull, on-scope shell).

### 5.0 What is shared with the `0x53` channel

`0x43` uses the **identical framing, checksum, and echo rules** as `0x53`
(see Section 3): `43 | len_LE16 | selector | args… | checksum`, `len = framelen − 3`,
`checksum = (Σ all preceding bytes) & 0xFF` (or the `0x66` wildcard), and every reply
echoes `selector | 0x80`. The only difference the device makes internally is a
**leader flag** it sets from byte 0 (1 for `0x43`, 0 for `0x53`), which routes the frame
to a separate 128-entry dispatch table. Multi-frame replies (file, shell) reuse the
same `01` = content / `02` = end subtype convention as `0x53/0x10`. [verified]

Selectors that are **defined** on the `0x43` table but not listed below (everything
outside the set here) return **no reply** (silent no-op). [verified]

### 5.1 Selector map (leader `0x43`)

| sel | name | OUT form | echo | reply | safety |
|---|---|---|---|---|---|
| `0x00` | FPGA config-register read | `43 05 00 00 <mode> <b> <c> ck` | `0x80` | regs as LE32 words | read-only [verified] |
| `0x01` | engine-sample debug read | `43 04 00 01 <n_LE16> ck` | `0x81` | up to 1024 samples | read-only [verified] |
| `0x02` | dump region 1 (config, 5072 B) | `43 02 00 02 ck` | `0x82` | single 5072-byte frame | read-only [verified] |
| `0x03` | dump region 2 (ADC cal, 8664 B) | `43 02 00 03 ck` | `0x83` | single 8664-byte frame | read-only [verified] |
| `0x10` | read file | `43 <len> 10 00 <path> ck` | `0x90` | content/end (same as `0x53/0x10`) | read-only [verified] |
| `0x11` | **shell exec (root)** | `43 <len> 11 <ascii cmd> ck` | `0x91` | stdout content/end | **DANGEROUS** [verified] |
| `0x40` | **write region 1** | `43 <len> 40 <bytes> ck` | `0xc0` | ack | **DESTRUCTIVE** [inferred] |
| `0x41` | **write region 2 (cal)** | `43 <0x21DA> 41 <8664 B> ck` | `0xc1` | ack | **DESTRUCTIVE** [inferred] |
| `0x42` | parameter setter | `43 05 00 42 <a> <v_LE16> ck` | `0xc2` | empty ack | not swept [gap] |
| `0x43` | repeat-action command | `43 04 00 43 <a> <cnt> ck` | `0xc3` | empty ack | not swept [gap] |
| `0x44` | beep | `43 03 00 44 <a> ck` | `0xc4` | empty ack | harmless [verified] |
| `0x45` | 8-byte misc write | `43 <len> 45 <id_LE16> <8 B> ck` | `0xc5` | empty ack | not swept [gap] |
| `0x50` / `0x60` | parameter I/O (mfg/debug) | `43 <len> 50/60 …` | `0xd0`/`0xe0` | — | not decoded [gap] |
| `0x7f` | commit / apply settings | `43 02 00 7f ck` | `0xff` | empty ack | safe (no reboot) [verified] |

The remainder of this section documents each selector byte-for-byte.

### 5.2 `0x00` — FPGA config-register read

Reads the acquisition FPGA's configuration register file. Three argument bytes follow
the selector: **`mode`, `b` (start register), `c` (count)**.

- **`mode == 1`** (bulk): returns registers `b .. b+c−1`, **each as a 4-byte
  little-endian word** (the underlying registers are 16-bit; the high half of each
  returned word reads back 0). The device enforces the bound **`b + c ≤ 0x6F` (111)**;
  violate it and you get **no reply**. [verified]
- **`mode == 0`** (single): reads one register at `b`. [verified]
- Any other `mode`: empty reply. [verified]

Reply length field = `2 + 4·c`. Example — read registers 0..3 (`mode=1, b=0, c=4`):

```
OUT  43 05 00 00 01 00 04 4d
IN   43 12 00 80 <r0 LE32> <r1 LE32> <r2 LE32> <r3 LE32> <ck>      # len 0x0012 = 2 + 16
```

The register file is **static configuration**: sweeping run → stop, **0 of the 110
registers changed value**. [verified] It is FPGA config/status, not a sample FIFO — do
not expect waveform data here.

### 5.3 `0x01` — engine-sample debug read

`n = LE16(args) >> 2`, clamped to **≤ 1024** samples. The bytes come **through the
normal acquisition engine** (the same source as `0x53/0x02`), so this is a *live rolling*
debug tap — it returns **nothing beyond the 3840-sample screen record**, not deep memory.
[verified]

```
OUT  43 04 00 01 00 10 58        # LE16 = 0x1000 = 4096 -> 4096>>2 = 1024 samples
IN   43 <len> 81 <samples…> <ck>
```

Use `0x53/0x02` (Section 4) for real acquisition; `0x01` is only an engine sanity tap.

> ⚠ **The count arg is mandatory.** Sending a **bare `0x43 0x01`** with no 2-byte LE
> count (payload just `01`) makes the firmware run `LE16()` on missing bytes → a
> garbage/huge sample count → it overruns the acquisition engine and the scope
> **crash-reboots** (USB I/O-error → re-enumeration; observed 2026-07-10, LA on). Always
> send the full `43 04 00 01 <n_LE16> ck`. And note this is the **same acquisition engine
> as `02 01 <ch>`** — it is *not* a separate LA-FPGA tap, so it is no route to a
> non-corrupting LA readout. `[verified]`

### 5.4 `0x02` / `0x03` — fixed region dumps

Two constant-size snapshots of the FPGA configuration/coefficient RAM. **Read-only, no
side effects.**

| sel | size | contents |
|---|---|---|
| `0x02` | **5072 B** (`0x13D0`) | region 1 — FPGA config/coefficient snapshot [verified] |
| `0x03` | **8664 B** (`0x21D8`) | region 2 — **ADC-lane linearization calibration RAM** [verified] |

```
OUT  43 02 00 02 47      IN  43 d2 13 82 <5072 bytes> <ck>     # len 0x13D2 = 5072 + 2
OUT  43 02 00 03 48      IN  43 da 21 83 <8664 bytes> <ck>     # len 0x21DA = 8664 + 2
```

The 8664-byte region-2 dump is **byte-identical to the on-disk calibration file**
retrievable with a file read (`/param/sav/chk1kb_091023`) — verified equal byte-for-byte
this session. [verified] It is the per-lane / per-code linearization LUT for the 8-lane
interleaved ADC (see the Calibration section). These two dumps are the *only* fixed-region
reads; they are **not** a deep-memory sample path.

### 5.5 `0x10` — read file

**Identical** to `0x53/0x10` (Section 4) but on the `0x43` leader; the reply still echoes
`0x90` and streams `90 01` content frames (≤ 10208 bytes each) + a `90 02 <sum8>` end
marker, no size frame, and accepts the `0x66` wildcard checksum. Use whichever leader is
convenient. [verified]

### 5.6 `0x11` — shell exec (root)

> ### ⚠️ SAFETY — `0x43/0x11` runs arbitrary commands as **root** on the scope
>
> - The payload is a shell command line executed on the instrument's Linux with **full
>   root privileges**. There is no sandbox. **A destructive command (`rm`, `mkfs`,
>   `dd`, overwriting `/dso_bin`, corrupting `/param/*`) can permanently brick the
>   scope.** Treat this selector as **read-only in practice** — inspection commands
>   (`cat`, `ls`, `ps`, `cat /proc/*`) only.
> - **A command that stalls / never returns will reboot the scope.** The instrument is
>   watched by a hardware dead-man timer and a software supervisor; if the shell blocks
>   long enough the supervisor kills and respawns the scope app (transient USB-link loss)
>   and the hardware watchdog can reset the whole SoC. **Never launch a blocking or
>   long-running command** (no `sleep`, no daemons, no `tail -f`, no unbounded `cat` of a
>   device node). [verified — observed reboot on a stalling command]
> - **No writes, no experiments.** Everything on this channel is unsupported and
>   untested by the vendor.

**How it works on the wire.** The command line is the ASCII payload after the selector
(`cmdlen = len − 2`). The device appends an output redirect to a **relative** message
file, runs the whole thing, then **streams that message file back exactly like a file
read** — content frames `43 <len> 91 01 <stdout…>` + end `43 04 00 91 02 <sum8>`,
echo `0x91`. [verified]

```
OUT  43 0e 00 11 63 61 74 20 2f 73 79 73 2e 69 6e 66 66     # "cat /sys.inf", 0x66 wildcard ck
IN   43 <len> 91 01 6d 6f 64 65 6c 3d …                     # stdout content
IN   43 04 00 91 02 <sum8> <ck>                             # end marker
```

**Two practical hazards from that "redirect to a relative message file" design:** [verified]

1. **One-behind message race.** Because the redirect target is a single fixed relative
   file and the reply streams *that file's current contents*, a fast command can be read
   back **one call behind** — you occasionally get the *previous* command's output. Guard
   against it by making each command **emit a unique marker** (e.g. append
   `; echo __DONE_<nonce>__`) and **retry the read** until the marker appears in the
   returned stream.
2. **Multi-command input must be a brace group.** Only the *last* shell segment lands in
   the redirected message file. To capture output from several commands in one call, wrap
   them so the redirect applies to the group — send `{ cmd1; cmd2; cmd3; }` as the payload
   (the device appends the redirect to the whole line), not `cmd1; cmd2; cmd3`.

### 5.7 `0x40` / `0x41` — region write (DESTRUCTIVE)

The write counterparts of `0x02`/`0x03`. `0x40` overwrites region 1 with the payload
bytes; `0x41` overwrites region 2 (the ADC cal RAM) and **only proceeds if you supply
the full 8664 bytes** (`len == 0x21DA`). These **were not swept** on hardware — writing
either region can leave the acquisition path miscalibrated or non-functional until a
factory cal is restored. **Do not use.** [inferred] echo `0xc0` / `0xc1`.

### 5.8 `0x42` / `0x43` / `0x44` / `0x45` — setters & beep

Small command selectors, each answered by an **empty ack** `43 02 00 <echo> ck`:

- **`0x44` — beep.** One argument byte selects the beep; harmless and handy as a
  round-trip liveness test. echo `0xc4`. [verified]
  ```
  OUT  43 03 00 44 01 8b        IN  43 02 00 c4 09
  ```
- **`0x42`** — parameter setter: `a` byte + a 16-bit LE value. echo `0xc2`. Effect not
  swept. [gap]
- **`0x43`** — key event with press and release: `43 <len> 43 <keycode> <cnt> ck`, echo `0xc3`
  (`43 02 00 c3 ck`). The argument byte is the **keycode**; a **release** is the keycode **OR-ed
  with `0x80`** (press = `keycode`, release = `keycode | 0x80`), `<cnt>` = repeat count. Softkeys
  are driven via the `0x13` inject instead (§9.3); this form is credited to the mikrocontroller.net
  *Hantek_Protokoll* article (DSO5xxxB family). See §9.3.1. `[external — mikrocontroller.net]`
- **`0x45`** — 16-bit id + an 8-byte payload write. echo `0xc5`. Effect not swept. [gap]
- **`0x50` / `0x60`** — parameter I/O (manufacturing/debug); argument layout not decoded,
  echo `0xd0` / `0xe0`. [gap]

### 5.9 `0x7f` — commit / apply settings

Persists the current settings to non-volatile storage (if dirty) and re-applies them,
then returns an **empty ack echoing `0xff`**. **This is NOT a reboot** — despite the name
it is sometimes given in third-party notes, no reset occurs; the scope keeps running.
[verified]

```
OUT  43 02 00 7f c4        IN  43 02 00 ff 44
```

Normal `0x53/0x11` settings writes already take effect live; `0x7f` is only needed if you
want a change to **survive a power cycle**. It is safe to call.

---

## 6. Waveform sample format

This section is the definitive decode of the sample bytes returned by the acquire
command (selector `0x02`, §4/§5). It answers exactly what each byte means, where it
sits on the screen, and how many bytes to expect. Read §5's acquire handshake first
for the frame envelope (size / data / end); here we open the payload.

> **Legend:** `[verified]` = seen on the wire or reproduced on hardware ·
> `[inferred]` = from cross-referenced captures / vendor manual · `[gap]` = unknown.

### 6.1 One glance

| property | value | tag |
|---|---|---|
| analog sample width | **1 byte**, two's-complement **signed int8** | `[verified]` |
| range on the wire | **−127 … +127** (`0x81` … `0x7F`), hard-clamped | `[verified]` |
| screen centre (0 V offset baseline) | `0x00` | `[verified]` |
| top rail (trace parked above screen) | `0x7F` (+127) | `[verified]` |
| bottom rail (trace parked below screen) | `0x81` (−127) | `[verified]` |
| trigger-column marker | one sample forced to `0xFF` | `[verified]` |
| `0x80` (−128) | **never occurs** (the clamp precludes it) | `[verified]` |
| vertical scale | **25 counts / division** | `[verified]` |
| vertical decode | `y_div = (int8(byte) − 16) / 25` (up = positive) | `[verified]` scale / `[gap]` the `+16` |
| horizontal density | **200 samples / division** | `[verified]` |
| block width | **3840** samples (or **3200** when a soft-menu panel is open) | `[verified]` |
| Logic-Analyzer sample width | **2 bytes**, little-endian 16-bit word, bit N = D(N) | `[verified]` |
| deep memory over USB | **not available** — screen block only | `[verified]` |

### 6.2 Analog samples are a signed int8 (this replaces the old "unsigned wrap" model)

Each analog sample byte (channels CH1 = `02 01 00`, CH2 = `02 01 01`, Math =
`02 01 02`) is a **two's-complement signed 8-bit integer** giving the sample's
vertical position **in counts, at 25 counts per division**, measured from a
fixed baseline. `[verified]`

```
s = byte - 256   if byte >= 128     # sign-extend the wire byte to a signed int8
    byte         otherwise
                                     #   0x00 -> 0   (screen centre baseline)
                                     #   0x7F -> +127 (top rail / clamp ceiling)
                                     #   0x81 -> -127 (bottom rail / clamp floor)
```

The device **clamps** every sample to `[-127, +127]` before transmitting, so:

- `0x7F` (+127) and `0x81` (−127) are the two saturation rails. A trace scrolled
  fully above the graticule reads a solid run of `0x7F`; fully below, a solid run of
  `0x81`. `[verified]`
- `0x80` (−128) can **never** appear — it is outside the clamp window. This is a
  reliable integrity check: a `0x80` in an analog data payload means a framing error,
  not a sample. Confirmed on hardware: **0 occurrences of `0x80`** across a full
  vertical-position sweep (`scope_dump/captures_wireshark/mso5202d-ch1-vpos.pcapng`, 138 240 sample bytes),
  and again this session across live CH1/CH2 reads. `[verified]`

**Session ground truth (2026-07-10):** a live capture of a centred signal read CH1
bytes in `0x26…0x55` (signed **+38 … +85**) and CH2 bytes in `0xD7…0x05`
(signed **−41 … +5**); no `0x80` bytes appeared; CH2 carried **exactly one** `0xFF`
sample = the trigger-column marker (§6.4). `[verified]`

#### Polarity: larger signed value = HIGHER on screen

The byte **increases as the trace moves up** on the display. Do **not** decode with
`128 − byte`; that inverts the motion (raise the trace and the plot descends). `[verified]`

#### Vertical decode (counts → divisions)

The sample byte already folds in the channel's vertical position (`VERT-CHx-POS`),
because the device draws the trace pre-positioned. To recover divisions-from-centre:

```
s      = int8(byte)            # signed, per above
y_div  = (s - 16) / 25         # +16 counts (~0.64 div) is a fixed baseline offset
                               # up = positive; each division = that channel's V/div
```

The `−16` removes a **≈0.64-division baseline bias**: with the channel centred
(`VERT-CHx-POS = 0`) the zero-signal baseline sits at byte `+16`, not `0x00`.
Whether this `+16` is a fixed instrument bias or an artifact of the position-zero
reference is **[gap]** — the `25` counts/div scale is solid, the `−16` offset is the
one un-nailed constant (see §7 for the absolute counts→volts offset, still open).

#### Equivalence to the position-unwrap form

Earlier tooling decoded via a position-aware unwrap:

```
base   = (VERT-CHx-POS + 16) & 0xFF
signal = ((byte - base + 128) mod 256) - 128       # AC part, in counts
y_div  = (VERT-CHx-POS + signal) / 25
```

Because the wire byte is `byte = POS + 16 + signal` clamped to a signed int8 (no
modular wrap survives the clamp), `signal = s − (POS + 16)` and the whole thing
collapses to `y_div = (s − 16)/25` — **identical result, no modulo arithmetic
needed.** `[verified]` The signed-int8 form is the canonical one; the unwrap form is
kept only to show equivalence.

#### RETRACTION — the old "unsigned mod-256 wrap / centre-hash / 0x0A–0xF2 rails" model is wrong

Prior documentation modeled the byte as **unsigned** `byte = (POS + 16 + signal) mod
256` with an "8-bit wrap producing a rail-to-rail hash near screen centre", and
treated bytes near `0x0A`/`0xF2` as **clipped rails**. All three claims were a
mis-reading of signed data and are **retracted**:

- **No wrap exists.** The clamp to `[−127, +127]` makes an 8-bit overflow impossible.
  The "hash near centre" was an unsigned reader seeing the signed-**zero crossing**:
  a small signal oscillating around 0 alternates `0xFF` (−1) ↔ `0x00` (0), which an
  unsigned decode reads as ~255 ↔ 0, i.e. a fake rail-to-rail toggle. Decoded signed,
  it is an ordinary small waveform. `[verified]`
- **`0x0A` / `0xF2` are NOT rails.** They are signed **+10 / −14** — a normal
  ~24-count (≈1-division) square wave straddling the signed-zero line. A rail detector
  keyed on "≈0 or ≈255" will falsely flag healthy on-screen signals as clipped. The
  **real** rails are `0x7F` (+127) and `0x81` (−127). `[verified]`
- **"Parked ≈ flat 129" was the bottom rail.** `0x81` = −127 is the clamp floor, not a
  mid-code idle. `[verified]`

> **[gap] — the genuine off-screen bimodal block.** Separately from the retraction
> above, there remains one *real* unexplained phenomenon: when a trace is dragged
> off-screen and back, a whole block can come back **split ~50/50 between `0x0A` and
> `0xF2`** with nothing in between (e.g. 1919 samples at `0x0A` + 1921 at `0xF2` in
> `scope_dump/captures_wireshark/mso5202d-ch1-vpos.pcapng`). This bimodal fill is **not** the `0x7F`/`0x81`
> clamp rails and is **not** predicted by the signed model. Treat such a block as
> "trace off-screen / invalid" and do not plot it as data. Its origin is unresolved.

### 6.3 Horizontal: 200 samples/division, block width tracks the display, not the timebase

- **200 samples per division** `[verified]`. Therefore `sample_interval = time_per_div / 200` and
  `sample_rate = 200 / time_per_div` for the screen block. (`decode_settings()` exposes
  `SAMPLE-INTERVAL-ns` = `TDIV/200` — a **screen-only** derivation; it is not clamped at the ADC
  max and is not the deep-record rate — read the deep rate from the CSV `#timebase`. The old
  `SAMPLERATE-HZ` derivation was removed as unused/misleading.)
- **Deep-record geometry** `[verified 2026-07-15]`: the acquired record spans **exactly 20
  divisions** with `record_len = 4000·mult` samples, `mult` ∈ {1, 5, 10, 100, 200} for
  4K / 20K / 40K / 512K / **1M** (so 1M = 4000·200 = **800000** samples, single-channel only — the
  "1,000,000" is the allocated buffer). Hence **deep samples/div = 200·mult**, **deep dt =
  TDIV/(200·mult)**, the window is `20·TDIV` (deep memory does NOT widen it — it multiplies the
  point count), and the Save→CSV file has `record_len + 64` rows (4064 / 40064 / 400064 / 800064).
  The timebase steps are **2-4-8 per decade**, index 0 = 2 ns/div (single-channel; dual-channel's
  fastest detent is 4 ns/div).
- The **block width is not fixed** and, crucially, **does not depend on the timebase**.
  It is governed by whether the right-hand soft-menu panel is open — the settings-blob
  field `CONTROL-DISP-MENU` (frame offset 206):

  | `CONTROL-DISP-MENU` | samples/block | plot width |
  |---|---|---|
  | 0 (no soft-menu panel) | **3840** | 19.2 div | `[verified]` |
  | 1 (menu panel open) | **3200** | 16.0 div | `[verified]` |

  Opening the panel shaves 3.2 divisions (640 samples ≈ 128 px) off the right of the
  plot. The timebase (`HORIZ-TB`) is unchanged across the transition — **it is the plot
  width that changes, not the sample rate.** `[verified]`
- **Never hard-code 3840.** Always read the sample count from the SIZE frame
  (§6.5). Interrupted reads have been observed to report short counts (e.g. 1537).
- The vendor app never reads more than one screen block per refresh; the largest single
  transfer it ever makes is the 3840-sample data frame (a 3847-byte frame). `[verified]`

### 6.4 Trigger-column marker and roll/scan fill (both use `0xFF`)

`0xFF` (−1) is overloaded on the wire and must be interpreted by context:

- **Trigger column:** exactly **one** sample in the block — the one aligned to the
  trigger instant — is forced to `0xFF`. Observed this session: CH2's block contained a
  single `0xFF` at the trigger column. `[verified]` (The internal renderer draws this as
  a vertical trigger tick.)
- **Roll / scan fill:** in roll or scan display modes a contiguous **sub-range** of the
  block is force-written `0xFF` to mean "not yet acquired". `[verified]` The filled span
  is a leading/trailing run rather than a single sample.
- **Ambiguity:** because `0xFF` is also the legitimate value −1 (one count below
  centre), a lone `0xFF` cannot be distinguished from a real −1 sample by value alone —
  only its position (single trigger column) or run-length (roll fill) disambiguates it.
  A decoder should special-case the trigger column and treat long `0xFF` runs as
  unacquired, but must not blanket-drop every `0xFF`.

### 6.5 SIZE frame — how to learn the length before reading the data

The acquire response begins with a SIZE frame (subtype `0x00`) that states the payload
length. Layout `[verified]`:

```
53 07 00 | 82 | 00 | <src> | <count byte0> <count byte1> <count byte2> | <ck>
          echo  sub   src            count, 24-bit little-endian
```

- `src` is the device's **internal** source id: **CH1 → 0, CH2 → 1, LA → 3**. For the
  Logic Analyzer this is `3` even though the request code and the DATA/END frames echo
  `5`. `[verified]`
- `count` is a **24-bit little-endian byte count of the DATA payload** (a fourth,
  most-significant byte is present in the buffer but is overwritten by the checksum, so
  only three bytes are usable — never an issue below 16.7 M). It is the **byte** count,
  so for analog `count == samples` but for LA `count == 2 × samples`. `[verified]`

Verified SIZE frames:

| device | frame | src | count | meaning |
|---|---|---|---|---|
| CH1 | `53 07 00 82 00 00 00 0f 00 eb` | 0 | `0x000f00` = 3840 | 3840 analog samples |
| CH2 | `53 07 00 82 00 01 00 0f 00 ec` | 1 | 3840 | 3840 analog samples |
| LA  | `53 07 00 82 00 03 00 19 00 f8` | 3 | `0x001900` = 6400 | 3200 LA samples × 2 B |

### 6.6 Full acquire payload — the three (or four) frames

For the acquire handshake envelope see §5; the payload-relevant facts:

| subtype | frame skeleton | meaning |
|---|---|---|
| `0x00` size | `53 07 00 82 00 <src> <count24> <ck>` | payload length (above) |
| `0x01` data | `53 <lenLE> 82 01 <ch> <count bytes> <ck>` | the samples; `len = count + 4` |
| `0x02` end  | `53 04 00 82 02 <ch> <ck>` | end-marker — **must be consumed** or the stream desyncs |
| `0x03` nodata | `53 04 00 82 03 <ch> <ck>` | "no fresh block" — treat as an empty/aborted read |

- Example CH1 data frame: `53 04 0f 82 01 00 <3840 bytes> <ck>` (`len 0x0f04 = 3844`). `[verified]`
- Example CH1 end frame: `53 04 00 82 02 00 db`. `[verified]`
- **Subtype `0x03` (no fresh data)** is a real fourth reply, emitted when the acquire
  poll finds no new block (this is the byte-level reason a too-deep store depth makes
  `0x02` appear to "stop responding": it is answering `82 03`, not hanging). Verified
  this session: an LA request while LA was idle returned `82 03 03` — subtype `0x03`,
  channel byte `3`, no data frame. `[verified]`

### 6.7 Logic Analyzer samples (channel code 5)

`02 01 05` returns the digital pod. `[verified]`

- **2 bytes per sample**, a **little-endian 16-bit word**: `word = raw[2i] |
  (raw[2i+1] << 8)`. `[verified]`
- **Bit N = digital channel D(N)** (D0 = LSB … D15 = MSB), the same bit order as
  `LA-CHANNEL-STATE`: `Dn(i) = (word >> n) & 1`. `[verified]`
- 3840 samples ⇒ 7680 bytes (or 3200 ⇒ 6400 when a menu panel is open). The SIZE frame
  reports the **byte** count (§6.5), so divide by 2 for the sample count. `[verified]`
- The vendor app **does** issue `02 01 05` over USB (contrary to earlier claims), but in
  every capture the returned words were idle-zero (LA inputs quiet), so the *usefulness*
  of the live LA-over-USB path is unresolved. `[gap]` The reliable way to view LA is the
  rendered screen framebuffer (selector `0x20`), which already contains the firmware-drawn
  D0–D15 rows.

### 6.8 What you cannot read over USB

- **No deep-memory readout.** The acquire command serves only the on-screen block
  (3840/3200 samples). Raising `ACQURIE-STORE-DEPTH` does not enlarge the USB read —
  beyond screen depth the device simply answers subtype `0x03` (no data). The vendor app
  never issues any larger or alternate read; the deep record is genuinely not exposed on
  the USB host link. `[verified]`
- To capture the deep record (40K/512K/…​) you must go through the file path: have the
  instrument write a CSV/reference file and read that file back over USB (selector
  `0x10`). See §7.4 for the CSV format and the depth-driven sample rate.

### 6.9 Open items for §6

- **[gap]** the genuine off-screen **bimodal `0x0A`/`0xF2` block** (§6.2) — not the
  clamp rails, not explained by the signed model.
- **[gap]** the absolute **counts → volts offset** and whether the `+16`-count baseline
  bias is a fixed instrument constant or a position-zero artifact (§7 gives the *scale*,
  `Vdiv/25`, exactly; the *offset* is the missing piece).

---

## 7. Calibration & counts → volts

Section 6 leaves samples in **counts** (25 counts/division). This section gives the
exact conversion the instrument itself uses to turn counts into volts, and documents the
analog front-end calibration pipeline that produces those counts. The conversion is
**[verified]** against ground-truth CSVs the instrument exports; the pipeline stages are
`[verified]` where a calibration file could be read back over USB and `[inferred]` where
only the on-screen effect is observable.

### 7.1 The exact conversion (matches the exported CSV to the digit)

```
volts = ( raw − zero_offset ) × Vdiv_uV_eff × probe / 25 000 000
```

| operand | meaning | units |
|---|---|---|
| `raw` | the sample value | counts (§6) |
| `zero_offset` | per-channel ADC zero / vertical-position reference | counts |
| `Vdiv_uV_eff` | effective volts/division, fine-vernier interpolated (§7.2) | **µV/div** |
| `probe` | probe attenuation, one of `{1, 10, 100, 1000}` | — |
| `25 000 000` | fixed constant | µV·count / (div·V) |

The constant factors cleanly: **`25 000 000 = 25 counts/div × 1 000 000 µV/V`**.
Substituting `Vdiv_uV_eff = Vdiv_V × 1 000 000`, everything cancels to

```
volts_per_count = Vdiv_V / 25          (exactly)
```

so **each count is `V/div ÷ 25` volts** — the same `DIV_UNIT = 25` the driver already
uses, now shown to be the ADC's own counts-per-division, not an approximation. `[verified]`

**Ground-truth check `[verified]`:** the exported `WaveData1410/1411.csv` were taken at
**5 V/div, 1× probe**. `5 V/div ÷ 25 = 0.200 V/count`. The CSV voltage column is entirely
multiples of **0.200** (`0.000, ±0.200, 1.000, 2.200, 3.000, −7.400, …`) → LSB = 0.2 V,
and the header reports `#voltbase=5000000` (= 5 V/div expressed in µV; see §7.4). The
formula reproduces the file exactly.

### 7.2 The V/div ladder and the Fine vernier

`Vdiv_uV_eff` starts from a **12-entry V/div ladder in microvolts** (this is the value
stored in the exported CSV's `voltbase` header, §7.4):

| V/div | 2 mV | 5 mV | 10 mV | 20 mV | 50 mV | 100 mV | 200 mV | 500 mV | 1 V | 2 V | 5 V | 10 V |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| µV/div | 2000 | 5000 | 10000 | 20000 | 50000 | 100000 | 200000 | 500000 | 1000000 | 2000000 | 5000000 | 10000000 |

This is exactly the driver's `VB_TO_MV` table × 1000. The 10 V/div entry is the
`VB = 0`-quirk range noted elsewhere. `[verified]`

When the **Fine (vernier)** vertical adjust is engaged (`VERT-CHx-FINE = 1`), the
effective V/div is interpolated toward the next-lower range:

```
Vdiv_uV_eff = table[vb] − fineV × table[vb−1] / 100
```

where `vb` is the V/div index and `fineV` is the vernier value. With the vernier at
zero, `Vdiv_uV_eff = table[vb]` (nominal). `[verified]` scale · `[gap]` the exact unit of
`fineV` (treated as 0..100 of a percent-style step).

### 7.3 The 8-lane ADC pipeline (how counts are produced)

The analog front-end is an **8-way time-interleaved ADC**: 8 lanes at **125 MHz each**
→ **1 GSa/s** single-channel (4 lanes/channel → 500 MSa/s dual), matching the vendor
manual's rate spec. `[inferred]` (manual + timing) Interleaving eight physical
converters introduces per-lane gain/offset mismatch, so the instrument applies a
two-stage per-lane correction before the sample reaches the host:

```
analog in
  → 8 interleaved ADC lanes (ad1 … ad8)
  → (1) per-lane DC offset      : add adc_off[lane]           (from /param/adc_off)
  → (2) per-lane, per-code LUT  : remap each 8-bit code through a 256×8 int8 table
                                   (linearization; enabled unless /linear_adc present)
  → linearized int16 sample, mid-scale-centred, 25 counts/division
  → clamp to signed int8 [−127, +127]  → the wire byte the host receives (§6)
```

Each stage corresponds to a calibration file that lives on the instrument filesystem and
can be read back over USB with the file-read command (selector `0x10`) — that readability
is why these are documented as observed artifacts rather than inference:

| file | role | format |
|---|---|---|
| `/param/adc_off` | **stage 1** — per-lane DC offset | INI keys `[ad1_off]`…`[ad8_off]`, one signed byte per lane `[inferred]` (self-cal generated) |
| `mult_adc.log` | **stage 2** — human-readable dump of the linearization table | ASCII, 3 blocks × 256 codes × 8 lanes of signed lane-correction values `[verified]` (readable) |
| `chk1kb_091023` | **stage 2** — the binary form loaded into the FPGA | **8664 bytes**: 32-B header + 2 KB reserved + three 256×8 int8 blocks (`HALF`, `CH1`, `CH2`) + a factory self-cal trailer `[verified]` |
| `/linear_adc` | **enable flag** for stage 2 | *presence* of this file **disables** linearization; **absence enables** it (counter-intuitive, verified from the loader behaviour) `[verified]` |

The three linearization blocks are `HALF` (reduced-rate interleave, fewer lanes active),
`CH1`, and `CH2` (full 8-lane per-channel tables). Each row maps a raw 8-bit code to a
linearized, mid-scale-centred signed value (monotone ≈ −116 at code 0 → +125 at code
255). `[verified]` The exact meaning of the 32-byte header and the self-cal trailer
records is `[gap]`.

The whole 8664-byte linearization image is also exposed on the command channel (leader
`0x43`, region-2 read) as an ADC-config dump — it is **cal data, not samples**; do not
mistake it for a deep-waveform readout. `[verified]`

### 7.4 Two more calibration inputs (gain trim; display path only)

- **4-point gain cal — `chk_base_volt`** (43-byte ASCII file, readable over USB): four
  keyed integers = the ADC code recorded for a known reference on four V/div ranges:
  ```
  [8mv]195
  [20mv]387
  [400mv]405
  [2000mv]2006
  ```
  These feed a per-range **software gain** used for the **on-screen / measurement**
  readout. `[verified]`
- **Per-range software gain trims** (≈1.0 ± a few %, e.g. 0.929 at 5 V/div, 0.899 at
  10 V/div) are applied on the **display / measurement** path only. `[inferred]`

> **Important:** the **CSV/save export path does NOT apply the software gain trims** —
> it uses the plain `(raw − zero_offset) × Vdiv_uV_eff × probe / 25e6` of §7.1. So a
> saved CSV is corrected only by the FPGA lane linearization, while the live on-screen
> amplitude additionally carries the ±few-% software trim. A saved-CSV amplitude may
> therefore differ from the screen readout by up to the trim (~7 % on the trimmed
> ranges). `[inferred]` — confirm on hardware.

### 7.5 The exported-CSV format (deep capture readout)

Because the USB acquire cannot serve deep memory (§6.8), the route to a long contiguous
record is: set the store depth, have the instrument save a CSV to its (internal) storage,
and read that file back over USB. One **Source** per file — the CSV menu Source selector
is CH1 / CH2 / **LA**, giving two file layouts:

**Analog (Source = CH1 / CH2)** — `time, volts`:

```
#timebase=<n>(ns)
,#voltbase=<n>(mv/100)      µV/div
#size=<N>
<time_s>,<volts>           ; N data rows: printf "%0.5E,%0.3f"
```

**Logic analyzer (Source = LA)** `[verified 2026-07-11]` — `time, word`:

```
#timebase=<n>(ns)
,#threshold=<n>(mv)        LA logic threshold, mV (e.g. 5482 = 5.482 V)
#size=<N>
<time_s>,<word>            ; word = the 16-bit LA sample, bit N = channel D(N)
```

The LA value column is the digital pod word (integer), **not** volts — bit N is channel
D(N). This is a **working route to real 16-channel LA data**, bypassing the broken live
`02 01 05` read (§5): capture with the pod on, Save→CSV with Source=LA, read the file
back. Confirmed genuine on hardware — per-bit toggle rates over one record match the
test-signal frequency ladder (D0 fastest → D15 slowest) exactly. **Caveat: enabling LA
forces the store depth to 4K** — deep memory (40K/512K/1M) is analog-only; with the pod
on, every depth clamps to 4K (verified by reading `ACQURIE-STORE-DEPTH` back). So LA CSV =
4064 samples; deep records are CH1/CH2 only.

Header decode (two of the labels are misleading — believe the numbers, not the units in
parentheses):

| header | printed as | actual meaning | example |
|---|---|---|---|
| `#timebase` | `%d(ns)` | the **screen time/div in picoseconds** — a constant tag, **NOT** the record's sample step. The `(ns)` label is wrong by 1000×: an export taken at 2 µs/div reports `2000000`, i.e. 2 µs = 2 000 000 ps, cross-checked against `HORIZ-WIN-TB` read back over `0x01` `[verified 2026-07-20]` | `2000000` = 2 µs/div |
| `#voltbase` | `%d(mv/100)` | actually **µV/div** → `V/div = voltbase / 1 000 000` (the `(mv/100)` label is wrong) `[verified]` | `5000000` = 5 V/div |
| `#size` | `%d` | number of data rows | `4064` / `40064` / `400064` |

- **The voltage column is volts already** (float, `%0.3f`), scope-computed via §7.1 —
  it side-steps the counts→volts conversion entirely for exported captures. `[verified]`
  (An earlier claim that the column held raw integer counts was mistaken.)
- **The time column is real seconds** (`%0.5E`), `time_s = i × dt`, where `dt` is the
  record's own sample step — **not** derived from the `#timebase` header. `[verified]`
- **Deeper memory samples faster**, and the step is depth-driven:

  | store depth | `#size` rows | sample step `dt` | sample rate |
  |---|---|---|---|
  | 4K | 4064 | **20 ns** | 50 MSa/s |
  | 40K | 40064 | **5 ns** | 200 MSa/s |
  | 512K | 400064 | **5 ns** | 200 MSa/s |

  So deep memory retains the fast rate rather than decimating to the screen; the record's
  `dt` comes from the depth, not from `time/div ÷ 200`. `[verified]`

- Note the **record length differs from the USB screen block**: the front-panel save
  record is **4064** samples (20.32 div) at the shallowest depth, versus the **3840**
  (or 3200) of the USB acquire — two distinct acquisition lengths, not a contradiction.
  `[verified]`
- The saved CSV is re-readable over USB via the file-read handshake (§4), fast enough
  that a 512K CSV (~7.7 MB) returns in ~10 s. `[verified]`

### 7.6 Open items for §7

- **[gap]** the absolute **counts → volts offset** / the `+16`-count baseline bias of
  §6.2 (the *scale* `Vdiv/25` is exact; the DC offset is not yet pinned).
- **[gap]** whether the exported CSV `voltbase` is multiplied by the probe factor when a
  10×/100× probe is selected (the `(mv/100)` label vs the observed µV magnitude should be
  re-checked on a fresh capture at a known V/div).
- **[gap]** whether a saved CSV's amplitude really differs from the on-screen readout by
  the software gain trim (§7.4).
- **[gap]** the exact unit of the Fine-vernier field (§7.2), and the semantics of the
  `chk1kb` 32-byte header and self-cal trailer (§7.3).
- **[gap]** `/param/adc_off` and `/linear_adc` are produced by on-instrument self-cal;
  their formats are documented from behaviour but were not observed as file data in the
  captures reviewed.

## 8. Settings-state blob (poll `0x01` / write `0x11`)

This is the heart of the device's control surface: **one 213-byte block that mirrors
the entire instrument front panel.** You read it with selector `0x01` and write it
back with selector `0x11`. The block is **byte-identical in both directions**, so all
host control is *read-modify-write*: poll the block, change the bytes you care about,
send it back. Every enum, unit, and offset below is drawn from the on-device field
list `/protocol.inf` (which declares `[TOTAL] 213` and one `[NAME] WIDTH` line per
field) and was confirmed by decoding the poll response while sweeping each menu on the
hardware (the `scope_dump/captures_wireshark/*.pcapng` menu-sweep set). [verified]

### 8.1 The two frames that carry the block

```
POLL request  (host → scope):  53 02 00 01 56
POLL response (scope → host):  53 D7 00 81 <213 param bytes> <ck>      (218 B total)
WRITE         (host → scope):  53 D7 00 11 <213 param bytes> <ck>      (218 B total)
WRITE ack     (scope → host):  53 03 00 91 <status> <ck>              (status 00=ok, FF=reject)
```

- `len` field = `0x00D7` = 215 = `framelen − 3` (1 selector byte + 213 params + no size prefix). [verified]
- `ck = Σ(bytes[0 .. framelen-2]) & 0xFF`, or the wildcard `0x66` (§3). [verified]
- The poll response echoes selector `0x01 | 0x80 = 0x81`; the write echoes `0x11 | 0x80 = 0x91`. [verified]
- **Write status byte:** `00` = accepted, `FF` = rejected (e.g. an out-of-range value or a `TRIG-SRC` disallowed for the current `TRIG-TYPE`). Only `00` observed on the wire; `FF` is [inferred].
- There is **no per-field addressing** — `0x11` always ships the whole block. To change one field: poll, patch, re-checksum, write. [verified]

### 8.2 Offset conventions

| column | meaning |
|---|---|
| **blk** | byte offset **within the 213-byte parameter block**, `0 … 212` (this is the `/protocol.inf` order) |
| **frm** | byte offset in the **raw USB frame** = `blk + 4` (the leader `53`, the LE16 length, and the `81`/`11` selector-echo occupy frame bytes 0…3) |

The block is **fully contiguous** — there are no reserved or padding bytes. `Σ width = 213`,
which exactly matches `/protocol.inf`'s `[TOTAL] 213`. [verified] All multi-byte fields
are **little-endian**. Signed fields (type `i16`/`i64`) are two's-complement; the complete
signed set is: `VERT-CH1-POS`, `VERT-CH2-POS`, `TRIG-VPOS`, `TRIG-SLOPE-V1`,
`TRIG-SLOPE-V2`, `HORIZ-TRIGTIME`, `LA-D7-D0-USER-THRESHOLD-VOLT`,
`LA-D15-D8-USER-THRESHOLD-VOLT`. **Every 8-byte TIME field is `int64` picoseconds**
(proof in §8.4). [verified]

### 8.3 The datasheet — all 213 bytes / 118 fields

**Tag legend:** `V` = [verified] on the wire / on hardware · `I` = [inferred] (cross-referenced
capture or panel label, not exercised over USB) · `G` = [gap] (function/units unknown).
`observed` = the literal value-set seen across all menu-sweep captures.

| field | blk | frm | w | type | units | enum (code → meaning) | range / observed | tag |
|---|--:|--:|--:|---|---|---|---|:--:|
| **VERT-CH1-DISP** | 0 | 4 | 1 | u8 | — | 0=hidden 1=shown. **A `0x11` write to this byte is ignored** — it does not enable/disable the channel or light its LED, and the field keeps its prior value. The channel is turned on/off only by the **CH1 button key event `0x13 18 <b>`** (keyid 24), which **toggles** `VERT-CH1-DISP` — each frame flips shown↔hidden (the `<b>` byte is a don't-care). To reach a desired state, read this field and send the key only if it needs to flip. [verified 2026-07-14] | {0,1} | V |
| **VERT-CH1-VB** | 1 | 5 | 1 | u8 | V/div idx | → `VB_TO_MV` (§8.4) | 0…10 | V |
| **VERT-CH1-COUP** | 2 | 6 | 1 | u8 | — | 0=DC 1=AC 2=GND | {0,1,2} | V |
| **VERT-CH1-20MHZ** | 3 | 7 | 1 | u8 | — | 0=Full 1=20 MHz BW-limit | {0,1} | V |
| **VERT-CH1-FINE** | 4 | 8 | 1 | u8 | — | 0=Coarse 1=Fine | {0,1} | V |
| **VERT-CH1-PROBE** | 5 | 9 | 1 | u8 | — | 0=1× 1=10× 2=100× 3=1000× | {0,1,2,3} | V |
| **VERT-CH1-RPHASE** | 6 | 10 | 1 | u8 | — | 0=Off 1=On (invert) | {0,1} | V |
| **VERT-CH1-CNT-FINE** | 7 | 11 | 1 | u8 | fine-gain step count | counter, active when `FINE=1` | {0,20,30} | G |
| **VERT-CH1-POS** | 8 | 12 | 2 | i16 | 1/25 div (25 cnt/div, up=+) | signed vertical position | −104…103 (≈±4 div seen; ±200=±8 div travel) | V |
| **VERT-CH2-DISP** | 10 | 14 | 1 | u8 | — | 0=hidden 1=shown | {0,1} | V |
| **VERT-CH2-VB** | 11 | 15 | 1 | u8 | V/div idx | → `VB_TO_MV` | 0…10 | V |
| **VERT-CH2-COUP** | 12 | 16 | 1 | u8 | — | 0=DC 1=AC 2=GND | {0,1,2} | V |
| **VERT-CH2-20MHZ** | 13 | 17 | 1 | u8 | — | 0=Full 1=20 MHz | {0,1} | V |
| **VERT-CH2-FINE** | 14 | 18 | 1 | u8 | — | 0=Coarse 1=Fine | {0,1} | V |
| **VERT-CH2-PROBE** | 15 | 19 | 1 | u8 | — | 0=1× 1=10× 2=100× 3=1000× | {0,1,2,3} | V |
| **VERT-CH2-RPHASE** | 16 | 20 | 1 | u8 | — | 0=Off 1=On (invert) | {0,1} | V |
| **VERT-CH2-CNT-FINE** | 17 | 21 | 1 | u8 | fine-gain step count | active when `FINE=1` | {0,20} | G |
| **VERT-CH2-POS** | 18 | 22 | 2 | i16 | 1/25 div | signed vertical position | −97…50 | V |
| **TRIG-STATE** | 20 | 24 | 1 | u8 | — | 0=STOP 1=WAIT/Ready 2=AUTO(untrig) 3=TRIG'D 4=SCAN/roll **5=SINGLE-CAPTURED(stopped, button red)** 6=re-arm | {0,1,2,3,5} seen; **4,6 = I** | V/I |
| **TRIG-TYPE** | 21 | 25 | 1 | u8 | — | 0=Edge 1=Video 2=Pulse 3=Slope 4=Overtime 5=Alter | {0…5} | V |
| **TRIG-SRC** | 22 | 26 | 1 | u8 | — | 0=CH1 1=CH2 2=EXT 3=EXT/5 4=AC-line. **Not writable via `0x11`** — a write is ignored and the source stays put (verified 2026-07-11); change it via the trigger menu keys instead. | {0…4}; set restricted per type (§8.5) | V |
| **TRIG-MODE** | 23 | 27 | 1 | u8 | — | 0=Auto 1=Normal | {0,1}; write mirrors to both `SWAP-CHx-MODE` | V |
| **TRIG-COUP** | 24 | 28 | 1 | u8 | — | 0=DC 1=AC 2=NoiseRej 3=HFRej 4=LFRej | {0…4} | V |
| **TRIG-VPOS** | 25 | 29 | 2 | i16 | 1/25 div of source | trigger LEVEL; volts = (VPOS−POS_src)·Vdiv/25 | −200…31000 (scales w/ V/div, §8.5) | V |
| **TRIG-FREQUENCY** | 27 | 31 | 8 | u64 | **milliHz** | HW freq counter of trig source (read-only); 1 000 000 = 1.000 kHz | 0…2 083 000 | V |
| **TRIG-HOLDTIME-MIN** | 35 | 39 | 8 | u64 | ps | **FIXED** capability bound (read-only) | 100000 (100 ns) | V |
| **TRIG-HOLDTIME-MAX** | 43 | 47 | 8 | u64 | ps | **FIXED** capability bound (read-only) | 10 000 000 000 000 (10 s) | V |
| **TRIG-HOLDTIME** | 51 | 55 | 8 | u64 | ps | live holdoff (Horizontal menu → F4); knob-push→100000 | 100 ns…10 s | V |
| **TRIG-EDGE-SLOPE** | 59 | 63 | 1 | u8 | — | 0=Rising 1=Falling | {0,1} | V |
| **TRIG-VIDEO-NEG** | 60 | 64 | 1 | u8 | — | 0=Normal 1=Inverted | {0,1} | V |
| **TRIG-VIDEO-PAL** | 61 | 65 | 1 | u8 | — | 0=NTSC 1=PAL/SECAM | {0,1} | V |
| **TRIG-VIDEO-SYN** | 62 | 66 | 1 | u8 | — | 0=AllLines 1=LineNum 2=OddField 3=EvenField 4=AllFields | {0…4} | V |
| **TRIG-VIDEO-LINE** | 63 | 67 | 2 | u16 | line # | used when `SYN=1`; 1…525 NTSC / 1…625 PAL | 1…525 seen (upper=I) | V |
| **TRIG-PULSE-NEG** | 65 | 69 | 1 | u8 | — | 0=Positive 1=Negative pulse | {0,1} | V |
| **TRIG-PULSE-WHEN** | 66 | 70 | 1 | u8 | — | 0='=' 1='≠' 2='>' 3='<' | {0…3} | V |
| **TRIG-PULSE-TIME** | 67 | 71 | 8 | u64 | ps | pulse width; min 20 000 (20 ns) … 10 s | ≥20000 | V |
| **TRIG-SLOPE-SET** | 75 | 79 | 1 | u8 | — | 0=Positive 1=Negative slope | {0,1} | V |
| **TRIG-SLOPE-WIN** | 76 | 80 | 1 | u8 | — | 0=V1(upper) 1=V2(lower) 2=Both | {0,1,2} | V |
| **TRIG-SLOPE-WHEN** | 77 | 81 | 1 | u8 | — | 0='=' 1='≠' 2='>' 3='<' | {0…3} | V |
| **TRIG-SLOPE-V1** | 78 | 82 | 2 | i16 | 1/25 div of source | upper threshold (same volts cal as level) | V1 > V2 (e.g. 47…77) | V |
| **TRIG-SLOPE-V2** | 80 | 84 | 2 | i16 | 1/25 div of source | lower threshold | −52…67 | V |
| **TRIG-SLOPE-TIME** | 82 | 86 | 8 | u64 | ps | slope time; min 20 000 (20 ns) … 10 s | ≥20000 | V |
| **TRIG-SWAP-CH1-TYPE** | 90 | 94 | 1 | u8 | — | 0=Edge 1=Video 2=Pulse 3=Overtime (**4-value**, no Slope/Alter) | {0…3} | V |
| **TRIG-SWAP-CH1-MODE** | 91 | 95 | 1 | u8 | — | 0=Auto 1=Normal (mirror of `TRIG-MODE`) | {0,1} | V |
| **TRIG-SWAP-CH1-COUP** | 92 | 96 | 1 | u8 | — | 0=DC 1=AC 2=NoiseRej 3=HFRej 4=LFRej | {0…4} | V |
| **TRIG-SWAP-CH1-EDGE-SLOPE** | 93 | 97 | 1 | u8 | — | 0=Rising 1=Falling | {0,1} | V |
| **TRIG-SWAP-CH1-VIDEO-NEG** | 94 | 98 | 1 | u8 | — | 0=Normal 1=Inverted | {0,1} | V |
| **TRIG-SWAP-CH1-VIDEO-PAL** | 95 | 99 | 1 | u8 | — | 0=NTSC 1=PAL/SECAM | {0,1} | V |
| **TRIG-SWAP-CH1-VIDEO-SYN** | 96 | 100 | 1 | u8 | — | 0…4 (= `TRIG-VIDEO-SYN`) | {0…4} | V |
| **TRIG-SWAP-CH1-VIDEO-LINE** | 97 | 101 | 2 | u16 | line # | as `TRIG-VIDEO-LINE` | 1…24 seen | V |
| **TRIG-SWAP-CH1-PULSE-NEG** | 99 | 103 | 1 | u8 | — | 0=Positive 1=Negative | {0,1} | V |
| **TRIG-SWAP-CH1-PULSE-WHEN** | 100 | 104 | 1 | u8 | — | 0…3 (=/≠/>/<) | {0…3} | V |
| **TRIG-SWAP-CH1-PULSE-TIME** | 101 | 105 | 8 | u64 | ps | min 20 000 (20 ns) | ≥20000 | V |
| **TRIG-SWAP-CH1-OVERTIME-NEG** | 109 | 113 | 1 | u8 | — | 0=Positive 1=Negative | {0,1} | V |
| **TRIG-SWAP-CH1-OVERTIME-TIME** | 110 | 114 | 8 | u64 | ps | min 20 000 (20 ns) | ≥20000 | V |
| **TRIG-SWAP-CH2-TYPE** | 118 | 122 | 1 | u8 | — | 0=Edge 1=Video 2=Pulse 3=Overtime | {0…3} | V |
| **TRIG-SWAP-CH2-MODE** | 119 | 123 | 1 | u8 | — | 0=Auto 1=Normal | {0,1} | V |
| **TRIG-SWAP-CH2-COUP** | 120 | 124 | 1 | u8 | — | 0…4 (coupling) | {0…4} | V |
| **TRIG-SWAP-CH2-EDGE-SLOPE** | 121 | 125 | 1 | u8 | — | 0=Rising 1=Falling | {0,1} | V |
| **TRIG-SWAP-CH2-VIDEO-NEG** | 122 | 126 | 1 | u8 | — | 0=Normal 1=Inverted | {0,1} | V |
| **TRIG-SWAP-CH2-VIDEO-PAL** | 123 | 127 | 1 | u8 | — | 0=NTSC 1=PAL/SECAM | {0,1} | V |
| **TRIG-SWAP-CH2-VIDEO-SYN** | 124 | 128 | 1 | u8 | — | 0…4 | {0…4} | V |
| **TRIG-SWAP-CH2-VIDEO-LINE** | 125 | 129 | 2 | u16 | line # | as `TRIG-VIDEO-LINE` | 0…525 seen | V |
| **TRIG-SWAP-CH2-PULSE-NEG** | 127 | 131 | 1 | u8 | — | 0=Positive 1=Negative | {0,1} | V |
| **TRIG-SWAP-CH2-PULSE-WHEN** | 128 | 132 | 1 | u8 | — | 0…3 | {0…3} | V |
| **TRIG-SWAP-CH2-PULSE-TIME** | 129 | 133 | 8 | u64 | ps | pulse width | ≥20000 | V |
| **TRIG-SWAP-CH2-OVERTIME-NEG** | 137 | 141 | 1 | u8 | — | 0=Positive 1=Negative | {0,1} | V |
| **TRIG-SWAP-CH2-OVERTIME-TIME** | 138 | 142 | 8 | u64 | ps | overtime | ≥20000 | V |
| **TRIG-OVERTIME-NEG** | 146 | 150 | 1 | u8 | — | 0=Positive 1=Negative | {0,1} | V |
| **TRIG-OVERTIME-TIME** | 147 | 151 | 8 | u64 | ps | overtime; min 20 000 … 10 s (SRC = CH1/CH2 only) | ≥20000 | V |
| **HORIZ-TB** | 155 | 159 | 1 | u8 | time/div idx | → `TB_TO_NS`; **acquisition TB, floors at idx 6 = 200 ns** | {6…31} | V |
| **HORIZ-WIN-TB** | 156 | 160 | 1 | u8 | time/div idx | → `TB_TO_NS`; knob/displayed TB, full range | {0…31} | V |
| **HORIZ-WIN-STATE** | 157 | 161 | 1 | u8 | — | window/zoom state — **always 0** in every capture | {0} | G |
| **HORIZ-TRIGTIME** | 158 | 162 | 8 | **i64** | **signed ps** | horizontal delay (neg = post-trigger); knob-push→0 | −4.06e9 … 5e13 | V |
| **MATH-DISP** | 166 | 170 | 1 | u8 | — | 0=off 1=on | {0,1} | V |
| **MATH-MODE** | 167 | 171 | 1 | u8 | — | 0=CH1+CH2 1=CH1−CH2 2=CH2−CH1 3=CH1×CH2 4=CH1/CH2 5=CH2/CH1 6=FFT | {0…6} | V |
| **MATH-FFT-SRC** | 168 | 172 | 1 | u8 | — | 0=CH1 1=CH2 | {0,1} | V |
| **MATH-FFT-WIN** | 169 | 173 | 1 | u8 | — | 0=Hanning 1=Flattop 2=Rectangular (**only 3 exist**) | {0,1,2} | V |
| **MATH-FFT-FACTOR** | 170 | 174 | 1 | u8 | — | 0=×1 1=×2 2=×5 3=×10 (FFT zoom) | {0…3} | V |
| **MATH-FFT-DB** | 171 | 175 | 1 | u8 | dB/div | 0=1 1=2 2=5 3=10 4=20 | {0…4} | V |
| **DISPLAY-MODE** | 172 | 176 | 1 | u8 | — | 0=Vectors 1=Dots | {0,1} | V |
| **DISPLAY-PERSIST** | 173 | 177 | 1 | u8 | — | 0=Auto 2=0.2 s 4=0.4 s 8=0.8 s 10=1.0 s 11=2.0 s 13=4.0 s 17=8.0 s 19=Infinity | {0,2,4,8,10,11,13,17,19} | V |
| **DISPLAY-FORMAT** | 174 | 178 | 1 | u8 | — | 0=XT 1=XY 2=FFT (FFT mode forces 2) | {0,1,2} | V |
| **DISPLAY-CONTRAST** | 175 | 179 | 1 | u8 | 0…15 | waveform intensity | {0…15} | V |
| **DISPLAY-MAXCONTRAST** | 176 | 180 | 1 | u8 | — | **FIXED 15** upper limit (read-only) | {15} | V |
| **DISPLAY-GRID-KIND** | 177 | 181 | 1 | u8 | — | 0=Off 1=Dotted 2=RealLine | {0,1,2} | V |
| **DISPLAY-GRID-BRIGHT** | 178 | 182 | 1 | u8 | 0…15 | grid intensity | {0…15} | V |
| **DISPLAY-MAXGRID-BRIGHT** | 179 | 183 | 1 | u8 | — | **FIXED 15** upper limit (read-only) | {15} | V |
| **ACQURIE-MODE** | 180 | 184 | 1 | u8 | — | 0=Normal 1=Peak 2=Average | {0,1,2} | V |
| **ACQURIE-AVG-CNT** | 181 | 185 | 1 | u8 | avg idx | → `ACQ_AVG_COUNTS` (0…5 = 4/8/16/32/64/128) | {0…5} | V |
| **ACQURIE-TYPE** | 182 | 186 | 1 | u8 | — | 0=Realtime 1=Equ-time | {0,1} | V |
| **ACQURIE-STORE-DEPTH** | 183 | 187 | 1 | u8 | — | 0=4K 4=40K 6=512K 7=1M (gaps 1/2/3/5 = greyed depths). Writing this via `0x11` changes the acquisition but **not** the Acquire-menu LongMem radio display (stays stale at 4K); the Acquire-menu **F5 softkey** (keyid 5) cycles it `4K→40K→512K→1M→(4K)` and *does* update the display. F5 advances **one step per key EDGE** (press `13 05 01` and release `13 05 00` each advance one, so two edges stretched apart in time double-count) — drive with single alternating edges + poll this field until it reaches the next step; set depth this way to keep the on-screen LongMem radio truthful | {0,4,6,7} | V |
| **MEASURE-ITEM1-SRC** | 184 | 188 | 1 | u8 | — | 0=CH1 1=CH2 3=LA (2 unused, no Math src) | {0,1,3} | V |
| **MEASURE-ITEM1** | 185 | 189 | 1 | u8 | — | measure type → §8.4 enum (0=Off) | {0…19 on wire} | V |
| **MEASURE-ITEM2-SRC** | 186 | 190 | 1 | u8 | — | 0=CH1 1=CH2 3=LA | {0,1,3} | V |
| **MEASURE-ITEM2** | 187 | 191 | 1 | u8 | — | measure type | ≤19 | V |
| **MEASURE-ITEM3-SRC** | 188 | 192 | 1 | u8 | — | 0=CH1 1=CH2 3=LA | {0,1,3} | V |
| **MEASURE-ITEM3** | 189 | 193 | 1 | u8 | — | measure type | ≤19 | V |
| **MEASURE-ITEM4-SRC** | 190 | 194 | 1 | u8 | — | 0=CH1 1=CH2 3=LA | {0,1,3} | V |
| **MEASURE-ITEM4** | 191 | 195 | 1 | u8 | — | measure type | ≤19 | V |
| **MEASURE-ITEM5-SRC** | 192 | 196 | 1 | u8 | — | 0=CH1 1=CH2 3=LA | {0,1,3} | V |
| **MEASURE-ITEM5** | 193 | 197 | 1 | u8 | — | measure type | ≤19 | V |
| **MEASURE-ITEM6-SRC** | 194 | 198 | 1 | u8 | — | 0=CH1 1=CH2 3=LA | {0,1,3} | V |
| **MEASURE-ITEM6** | 195 | 199 | 1 | u8 | — | measure type | ≤19 | V |
| **MEASURE-ITEM7-SRC** | 196 | 200 | 1 | u8 | — | 0=CH1 1=CH2 3=LA | {0,1,3} | V |
| **MEASURE-ITEM7** | 197 | 201 | 1 | u8 | — | measure type | ≤19 | V |
| **MEASURE-ITEM8-SRC** | 198 | 202 | 1 | u8 | — | 0=CH1 1=CH2 3=LA | {0,1,3} | V |
| **MEASURE-ITEM8** | 199 | 203 | 1 | u8 | — | measure type | ≤19 | V |
| **CONTROL-TYPE** | 200 | 204 | 1 | u8 | — | menu-event kind — **always 0** in every capture | {0} | G |
| **CONTROL-MENUID** | 201 | 205 | 1 | u8 | — | active on-screen menu id (full map in §9) | {1…63} | V |
| **CONTROL-DISP-MENU** | 202 | 206 | 1 | u8 | — | 0=menu closed 1=menu shown; **governs acquire width 3840↔3200** (§ acquire) | {0,1} | V |
| **LA-SWI** | 203 | 207 | 1 | u8 | — | 0=LA off 1=LA on | {0,1} | V |
| **LA-CHANNEL-STATE** | 204 | 208 | 2 | u16 | bitmask | bit N = D(N) enabled (D0=LSB; lo byte=D0–7, hi byte=D8–15; all-on=0xFFFF) | 0…65535 | V |
| **LA-CURRENT-CHANNEL** | 206 | 210 | 1 | u8 | — | selected digital channel 0…15 | {0…15} | V |
| **LA-D7-D0-THRESHOLD-TYPE** | 207 | 211 | 1 | u8 | — | 0=TTL 1=CMOS 2=ECL 3=User | {0…3} | V |
| **LA-D15-D8-THRESHOLD-TYPE** | 208 | 212 | 1 | u8 | — | 0=TTL 1=CMOS 2=ECL 3=User | {0…3} | V |
| **LA-D7-D0-USER-THRESHOLD-VOLT** | 209 | 213 | 2 | i16 | volts = raw/4096 | user thr (12-bit DAC = code<<4, raw%16==0; ±8 V, ≈3.9 mV step); used when type=3 | −32336…32656 | V |
| **LA-D15-D8-USER-THRESHOLD-VOLT** | 211 | 215 | 2 | i16 | volts = raw/4096 | user thr; used when type=3 | −32336…32656 | V |

Block ends at **blk 213 / frm 217** (the checksum byte follows). `Σ width = 213`. ✓

> **Worked decode example.** A poll response begins
> `53 D7 00 81 | 01 09 00 01 01 00 00 00 CA 00 …` The first param byte `01` (blk 0) =
> `VERT-CH1-DISP` = shown; `09` (blk 1) = `VERT-CH1-VB` = **2 V/div**; `00 00`... then at
> blk 8 the LE i16 `CA 00` = `VERT-CH1-POS` = +202 (off-scale high in that grab). To change
> only CH1 coupling to AC: patch frm byte 6 (blk 2) to `01`, recompute `ck`, and send the
> full block back as `53 D7 00 11 …`. [verified]

### 8.4 Conversion tables (every code)

**`VB_TO_MV`** — `VERT-CHx-VB` index → mV/div. Verified end-to-end by stepping the V/div knob in
`mso5202d-ch1-vdiv.pcapng` / `-ch2-vdiv.pcapng`.

| idx | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |
|---|--|--|--|--|--|--|--|--|--|--|--|
| mV/div | 2 | 5 | 10 | 20 | 50 | 100 | 200 | 500 | 1000 | 2000 | 5000 |

> **VB = 0 quirk.** The knob ladder wraps `… → 9 (2 V) → 10 (5 V) → 0 (2 mV) → 1 …`, i.e. **10 V/div
> also lands on index 0**. So index `0` is ambiguous — it means 2 mV/div *or* 10 V/div (12 physical
> sensitivities on 11 codes). Disambiguate from `TRIG-VPOS` magnitude when a level is set (a 2 mV/div
> level pushes `VPOS` into the thousands; §8.5), or from the trace scale. The stored `VB` index is
> unaffected by `VERT-CHx-PROBE` (probe scales only the *displayed* V/div label). [verified]

**`TB_TO_NS`** — `HORIZ-TB` / `HORIZ-WIN-TB` index → ns/div. A **2-4-8 decade ladder**, 32 steps,
verified in `mso5202d-timediv.pcapng`.

| idx | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 | 12 | 13 | 14 | 15 |
|--|--|--|--|--|--|--|--|--|--|--|--|--|--|--|--|--|
| /div | 2 ns | 4 ns | 8 ns | 20 ns | 40 ns | 80 ns | 200 ns | 400 ns | 800 ns | 2 µs | 4 µs | 8 µs | 20 µs | 40 µs | 80 µs | 200 µs |
| **idx** | **16** | **17** | **18** | **19** | **20** | **21** | **22** | **23** | **24** | **25** | **26** | **27** | **28** | **29** | **30** | **31** |
| /div | 400 µs | 800 µs | 2 ms | 4 ms | 8 ms | 20 ms | 40 ms | 80 ms | 200 ms | 400 ms | 800 ms | 2 s | 4 s | 8 s | 20 s | 40 s |

> `HORIZ-WIN-TB` (the knob/displayed timebase) covers the full 0…31. **`HORIZ-TB` (the *acquisition*
> timebase) floors at idx 6 = 200 ns** — for knob positions 0…5 (2…80 ns/div) the acquisition stays at
> idx 6 while the display digitally zooms in (observed set `{6…31}`, never below 6; at the boundary
> `HORIZ-TB` may briefly lead by one index). This is why the fast end reaches 2 ns/div on screen but the
> sampler never runs finer than 200 ns/div-equivalent. [verified]

**`ACQ_AVG_COUNTS`** — `ACQURIE-AVG-CNT` index → averages, `= 4·2ⁿ`:

| idx | 0 | 1 | 2 | 3 | 4 | 5 |
|---|--|--|--|--|--|--|
| averages | 4 | 8 | 16 | 32 | 64 | 128 |

**MEASURE type enum** — `MEASURE-ITEMn`. Codes **0–19 are [verified]** on the wire (observed set
{0-11,13,17,19} across the `mso5202d-measure.pcapng` ITEM-8 sweep). Codes **12, 14, 15, 16, 18 and
20–31 are [inferred]** from on-screen menu labels only — never seen over USB.

| code | meaning | tag | code | meaning | tag |
|--:|---|:--:|--:|---|:--:|
| 0 | Off | V | 16 | Vbase | I |
| 1 | Frequency | V | 17 | Vtop | V |
| 2 | Period | V | 18 | Vmid | I |
| 3 | Mean | V | 19 | Vamp | V |
| 4 | Pk-Pk | V | 20 | Overshoot | I |
| 5 | Cyc RMS | V | 21 | Preshoot | I |
| 6 | Minimum | V | 22 | Period Mean | I |
| 7 | Maximum | V | 23 | Period RMS | I |
| 8 | Rise Time | V | 24 | FOvershoot | I |
| 9 | Fall Time | V | 25 | RPreshoot | I |
| 10 | +Width | V | 26 | Burst Width | I |
| 11 | −Width | V | 27 | FRF | I |
| 12 | Delay1-2 Rise | I | 28 | FFR | I |
| 13 | Delay1-2 Fall | V | 29 | LRR | I |
| 14 | +Duty | I | 30 | LRF | I |
| 15 | −Duty | I | 31 | LFR | I |

**The picosecond ladder (proof every 8-byte TIME field is `int64` ps, not IEEE-754 double).** The
`TIME`/`HOLDTIME`/`TRIGTIME` fields on the wire are always clean base-10 integers drawn from a 1-2-5
decade ladder spanning 5 ns … 10 s: `5000, 10000, 20000, 50000, 100000, 200000, 500000, 1e6, 2e6, 5e6,
1e7, 2e7, 5e7, 1e8, 2e8, 5e8, 1e9, 2e9, 5e9, 1e10 … 1e13`. Concretely, a 500 ns pulse width serializes
as `TRIG-PULSE-TIME = 500000` → LE bytes `20 A1 07 00 00 00 00 00`; the 10 s holdoff ceiling is
`10000000000000` → `00 A0 72 4E 18 09 00 00`. If these were IEEE-754 doubles, `500000.0` would instead
appear as `00 00 00 00 80 84 1E 41` (= the integer 4692333547057315840). The wire shows the plain
integer, so the encoding is unambiguously **`int64` picoseconds**. Minimum-width fields floor at
`20000` (20 ns). [verified]

### 8.5 Field-behaviour notes

- **`TRIG-MODE` fans out.** A single `TRIG-MODE` change writes `TRIG-MODE`, `TRIG-SWAP-CH1-MODE`, and
  `TRIG-SWAP-CH2-MODE` to the same value **in one frame** (seen 0→1 together in `mso5202d-trig-edge.pcapng`
  frame 208, and 1→0 in frame 218). Treat them as one logical field; when writing, set all three. The
  untriggered `TRIG-STATE` follows: Auto→2, Normal→1. [verified]

- **`VERT-CHx-RPHASE = 1` reflects the trigger level.** Inverting the *source* channel negates the signed
  trigger level about `POS_src`. Exact pair from `mso5202d-ch1-menu.pcapng` (CH1 POS=68, VB=10=5 V/div):
  `RPHASE 0 → TRIG-VPOS 81` (81−68 = +13 → +2.6 V) flips to `RPHASE 1 → TRIG-VPOS 55` (55−68 = −13 → −2.6 V).
  Inverting the non-source channel does nothing to the level. [verified]

- **Knob-push resets** (from `mso5202d-pos-knob-push.pcapng` / `-trig-knob.pcapng`):
  vertical position push → `VERT-CHx-POS = 0` (centre); horizontal position push → `HORIZ-TRIGTIME = 0`;
  holdoff knob push → `TRIG-HOLDTIME = 100000` (100 ns); trigger-level knob push → `TRIG-VPOS := POS_src`
  (level = 0 V / channel ground). Distinct from the **"Set 50 %"** softkey, which snaps `TRIG-VPOS` to the
  signal's mid-amplitude. [verified]

- **`TRIG-VPOS` scales with V/div — it is NOT clamped to ±200.** The field stores the level in 1/25-div of
  the *current* V/div, and the instrument rescales it when V/div changes so the *absolute volts* are
  preserved. Holding a fixed +2.480 V level while stepping V/div (from `mso5202d-ch1-vdiv.pcapng`,
  CH1 POS=0):

  | VB | V/div | TRIG-VPOS | VPOS·Vdiv/25 |
  |--:|--|--:|--|
  | 9 | 2 V | 31 | 2480 mV |
  | 8 | 1 V | 62 | 2480 mV |
  | 7 | 500 mV | 124 | 2480 mV |
  | 6 | 200 mV | 310 | 2480 mV |
  | 5 | 100 mV | 620 | 2480 mV |
  | 0 | 2 mV | 31000 | 2480 mV |

  So `TRIG-VPOS` reaches **~31000 at 2 mV/div**. The familiar **±200 = ±8 div** figure is only the
  on-screen *knob-travel* limit at a fixed V/div, not the field's range. Volts (CH1/CH2 sources):
  `level_V = (TRIG-VPOS − POS_src) × Vdiv/25`, where `POS_src = VERT-CH{src}-POS`. `TRIG-SLOPE-V1/V2` use
  the identical calibration. (EXT/EXT-5/AC-line level→volts is not derivable from the blob — [gap].) [verified]

- **`TRIG-SRC` is restricted by `TRIG-TYPE`.** Edge = all 5 (CH1/CH2/EXT/EXT-5/AC-line);
  Video/Pulse/Slope = CH1/CH2/EXT/EXT-5 (no AC-line); **Overtime = CH1/CH2 only** (confirmed:
  `mso5202d-trig-overtime.pcapng` SRC ∈ {0,1}). A write with a disallowed source is expected to be
  rejected with write-status `FF`. [verified / rejection = inferred]

- **Alter mode leaves the main sub-params stale — read the SWAP blocks.** When `TRIG-TYPE = 5` (Alter),
  the *main* Video/Pulse/Slope/Overtime fields are no longer maintained and read back garbage. In
  `mso5202d-trig-type.pcapng` the main `TRIG-PULSE-TIME` reads **5652756404163837952**
  (`00 00 00 00 00 A0 72 4E` LE = `0x4E72A00000000000`) — a byte-shifted image of the 10 s holdtime-max
  constant `10000000000000` (`00 A0 72 4E 18 09 00 00`; note the shared `A0 72 4E`), i.e. a serializer
  artifact, **not** a real time. In Alter mode, read each channel's active config from the
  `TRIG-SWAP-CHx-*` blocks and **treat any TIME field > 1e13 ps (10 s) as invalid**. [verified]

- **Alter menu-id ↔ type order caveat.** In Alter, `CONTROL-MENUID` encodes both channel and type, but
  its order differs from the `TRIG-SWAP-*-TYPE` code order: CH1 menu ids `26/27/28/29` =
  Edge/Pulse/Video/Overtime → type codes `0/2/1/3`; CH2 ids `30/31/32/33` likewise (24 = Alter base).
  Verified on both channels. [verified]

- **Read-only / constant fields** (echo hardware capability, never settable, constant across all 44
  captures): `TRIG-HOLDTIME-MIN = 100000` (100 ns), `TRIG-HOLDTIME-MAX = 10000000000000` (10 s),
  `DISPLAY-MAXCONTRAST = 15`, `DISPLAY-MAXGRID-BRIGHT = 15`. Writing them has no effect. [verified]

- **Always-0 / unmapped fields:** `HORIZ-WIN-STATE` (blk 157) and `CONTROL-TYPE` (blk 200) held `0` in
  every capture — even in the horizontal-window / zoom menus — so their function is unknown. [gap]
  `VERT-CHx-CNT-FINE` (blk 7/17) is a fine-gain step counter active when `FINE=1` (observed {0,20,30});
  its magnitude/units are unmapped. [gap]

- **`MATH-FFT-WIN` has exactly 3 windows.** The FFT-window knob cycles `0→1→2→0` only in
  `mso5202d-math.pcapng` (value-set {0,1,2}); there is **no** Bartlett (3) or Blackman (4) on this
  instrument — any earlier 5-window claim was speculative. [verified]

- **`TRIG-FREQUENCY` is milliHz.** For the 1 kHz calibration signal the field reads `1000000`
  (mHz → Hz = field/1000); it reads `0`/noise when the source is not triggering. Read-only. [verified]

- **`CONTROL-DISP-MENU` shrinks the acquire record.** Opening any soft-menu (`CONTROL-DISP-MENU` 0→1)
  drops the acquire SIZE-frame count from 3840 to 3200 samples (the menu overlay shaves 3.2 div off the
  plot width); the timebase does not change the count. See the acquire section for the SIZE-frame detail. [verified]

## 9. Menus & front-panel keys

Everything a human does at the front panel — pressing a bezel softkey, turning a
knob, opening a menu — is reachable over USB through **one selector: `0x13`**
(key events). There is no "open menu X" or "save waveform" USB command; the host
reproduces panel actions by injecting the *same* key events the physical buttons
generate. Two on-device files describe the mapping and are themselves readable
over USB with the file-read selector `0x10`:

- **`/keyprotocol.inf`** — the 49-entry key list; a key is named by its **0-indexed
  position** in this file (that index is the `keyid` you put in a `0x13` frame).
- **`/protocol.inf`** — the settings-blob layout; its `CONTROL-MENUID` field (raw
  offset 201, wire offset 205) reports *which menu is currently open*, letting the
  host see the panel's menu state via the `0x01` poll.

### 9.1 `CONTROL-MENUID` — which menu is open

`CONTROL-MENUID` is a single byte in the settings blob (poll `0x01` / write `0x11`).
Reading it tells you the active menu; the companion byte `CONTROL-DISP-MENU` (raw
202 / wire 206) is `1` while a menu is visible and `0` when the menu bar is closed.
The full id→menu map, assembled by opening each menu and polling the blob
(`scope_dump/captures_wireshark/mso5202d-utility.pcapng`, `-la-*.pcapng`, and the per-menu captures):

| id | menu | id | menu |
|---:|------|---:|------|
| 1  | CH1 vertical | 24 | Trigger → Alter (base) |
| 2  | CH2 vertical | 25 | Default Setup (factory reset) |
| 3  | Horizontal page 1 (window / marks) | 26/27/28/29 | Alter → CH1 Edge/Pulse/Video/Overtime |
| 4  | Display page 1 (Type/Persist/Contrast) | 30/31/32/33 | Alter → CH2 Edge/Pulse/Video/Overtime |
| 5  | Trigger → Edge | 36 | Display page 2 (Grid / Format) |
| 6  | Trigger → Pulse page 1 | 38 | Trigger → Overtime page 1 |
| 7  | Trigger → Pulse page 2 (When/Time) | 39 | Trigger → Overtime page 2 (Coupling) |
| 8  | Trigger → Video | 40 | Horizontal page 2 (holdoff / coarse-fine) |
| 10 | default / no active menu — **also** Utility page 3 (reuses this id) | 41 | Math operations (`MATH-DISP`) |
| 11 | Trigger base (level / Edge default) | 42 | Utility page 1 (sys-status/update/save-wave/self-cal) |
| 15 | Cursor (cursor state is **not** in the blob) | 43 | Utility page 2 |
| 16 | Math → FFT page 1 (source / window) | 47 | Save/Recall base (type selector) |
| 17 | Acquire | 48 | Save/Recall → CSV **and** FileList (shared browser) |
| 18 | Save/Recall → SETUP | 56 | Math → FFT page 2 (zoom / vertical scale) |
| 19 | Save/Recall → REF | 61 | Logic Analyzer base (`LA-SWI`) |
| 20 | Measure base | 62 | LA config → D7-D0 group |
| 21 | Measure → item add/config (`MEASURE-ITEM*`) | 63 | LA config → D15-D8 group |
| 22 | Trigger → Slope page 1 |  |  |
| 23 | Trigger → Slope page 2 (V1/V2/When/Time) |  |  |

Notes and caveats:

- **Constant-while-open menus.** Some menus set `CONTROL-MENUID` when opened and
  keep it constant while open, so in a poll they show up as a steady *value* rather
  than a *change* — CH1 = 1, CH2 = 2, Acquire = 17 behave this way; only
  `CONTROL-DISP-MENU` toggles 0↔1. [verified]
- **Reused id 10.** Utility page 3 gets no dedicated id and reuses `10`, the same
  value as the no-menu baseline (distinguished only by `CONTROL-DISP-MENU`=1). The
  three Utility pages cycle `42 → 43 → 10`. [verified]
- **Multi-page trigger submenus use consecutive ids** for page 1 / page 2 (Pulse
  6/7, Slope 22/23, Overtime 38/39). [verified]
- **Menus with NO blob footprint** (so they cannot be observed through the poll):
  Cursor (15), the Horizontal window/marks controls, Display refresh-rate,
  Save/Recall (Storage), and Utility — these place *no* parameters in the settings
  blob; their softkeys are pure actions. [verified]
- **Candidate deeper ids 58 and 69** (Utility / self-cal sub-pages) are suspected
  but were **not** seen in any capture. [gap]
- Remaining unmapped ids (0, 9, 12, 13, 14, 34, 35, 37, 44–46, 49–55, 57–60) stay
  unassigned until each corresponding page is opened on hardware. [gap]

### 9.2 `/keyprotocol.inf` — the 49 front-panel keys

`[TOTAL] 49`, one CRLF-terminated name per line between `[START]`/`[END]`. The
**line index (0-based) is the `keyid`** you send in `0x13 | keyid | <b>` (the second byte `<b>`
is a don't-care, §9.3). The full list (byte-exact from the on-device file):

| id | name | meaning |
|---:|------|---------|
| 0–7 | FN-0-KEY … FN-7-KEY | bezel softkeys F1…F8 (menu option keys) |
| 8 | FN-MLEFT-KEY | multipurpose knob turn − / prev |
| 9 | FN-MRIGHT-KEY | multipurpose knob turn + / next |
| 10 | FN-MZERO-KEY | multipurpose knob push (zero / select) |
| 11 | MENU-SR-KEY | Save/Recall menu |
| 12 | MENU-MEASURE-KEY | Measure menu |
| 13 | MENU-ACQUIRE-KEY | Acquire menu |
| 14 | MENU-UTILITY-KEY | Utility menu |
| 15 | MENU-CURSOR-KEY | Cursor menu |
| 16 | MENU-DISPLAY-KEY | Display menu |
| 17 | CT-AUTOSET-KEY | **Autoset** |
| 18 | CT-SINGLESEQ-KEY | Single sequence |
| 19 | CT-RS-KEY | **Run/Stop** |
| 20 | CT-HELP-KEY | Help |
| 21 | CT-DS-KEY | **Default Setup** (factory reset) |
| 22 | CT-STU-KEY | Setup ("STU") key |
| 23 | VT-MATH-MENU-KEY | Math menu |
| 24 | VT-CH1-MENU-KEY | CH1 on-off — **toggles** `VERT-CH1-DISP`: each `13 18 <b>` frame flips CH1 shown↔hidden (`<b>` is a don't-care). The `VERT-CH1-DISP` settings byte (§8) tracks the resulting state but is not settable via `0x11`; read it and send the key only when a flip is needed. [verified 2026-07-14] |
| 25 | VT-CH1-PSUB-KEY | CH1 vertical position − |
| 26 | VT-CH1-PADD-KEY | CH1 vertical position + |
| 27 | VT-CH1-PZERO-KEY | CH1 position knob push (`VERT-CH1-POS := 0`) |
| 28 | VT-CH1-VBSUB-KEY | CH1 V/div − |
| 29 | VT-CH1-VBADD-KEY | CH1 V/div + |
| 30 | VT-CH2-MENU-KEY | CH2 on-off — **toggles** `VERT-CH2-DISP` the same way as CH1 (each `13 1e <b>` frame flips shown↔hidden). Default Setup baseline = CH1 on, CH2 off. [verified 2026-07-14] |
| 31 | VT-CH2-PSUB-KEY | CH2 vertical position − |
| 32 | VT-CH2-PADD-KEY | CH2 vertical position + |
| 33 | VT-CH2-PZERO-KEY | CH2 position knob push (`VERT-CH2-POS := 0`) |
| 34 | VT-CH2-VBSUB-KEY | CH2 V/div − |
| 35 | VT-CH2-VBADD-KEY | CH2 V/div + |
| 36 | HZ-MENU-KEY | Horizontal menu |
| 37 | HZ-PSUB-KEY | Horizontal delay − |
| 38 | HZ-PADD-KEY | Horizontal delay + |
| 39 | HZ-PZERO-KEY | Horizontal position push (`HORIZ-TRIGTIME := 0`) |
| 40 | HZ-TBSUB-KEY | SEC/DIV − (slower) |
| 41 | HZ-TBADD-KEY | SEC/DIV + (faster) |
| 42 | TG-MENU-KEY | Trigger menu |
| 43 | TG-PSUB-KEY | Trigger level − |
| 44 | TG-PADD-KEY | Trigger level + |
| 45 | TG-PZERO-KEY | Trigger level knob push (`TRIG-VPOS := ground`) |
| 46 | TG-PHALF-KEY | **Trigger Set-50 %** |
| 47 | TG-FORCE-KEY | **Force trigger** |
| 48 | TG-PROBECHECK-KEY | Probe check / probe compensation |

`FN-0..7` are the eight bezel option keys (which softkey is which depends on the
currently-open menu, §9.1). The five bold ids (17/19/21/46/47) match the earlier
Appendix-F control keys exactly. [verified against the on-device file]

**Driving the knobs to set values.** The ± knob keys (V/div 28/29 & 34/35, SEC/DIV
40/41, position 25/26 & 31/32, trigger level 43/44) each step the value one notch;
inject one, poll the resulting field over `0x01`, repeat until the read-back reaches
the target (V/div → `CHn-VDIV-mV`, SEC/DIV → `SAMPLE-INTERVAL-ns`×200, level →
`TRIG-VPOS`). This closed loop is how a driver sets controls **without** an `0x11`
write (§4 `0x11` — a raw field write skips key side-effects). Two verified quirks
[2026-07-15]: **SEC/DIV 40/41 are inverted** vs their SUB/ADD names on this firmware
(40 = faster, 41 = slower) — drive by read-back, not by the label; and **Set-50 %
(id 46) is a no-op over USB injection** — it does not move `TRIG-VPOS` even with the
scope running and TRIG'D, though the *physical* key works. The trigger level does not
need setting for a ground-referenced logic signal anyway: with channel POS = 0 the
3.3 V signal's low rail sits at 0 V = screen centre, so the DS-default `TRIG-VPOS` 0
already triggers it. `push` ids (27/33 position, 45 trigger level) zero their axis.

### 9.3 How a `0x13` key press flows — and the single-slot mailbox

Frame form (leader `0x53`): `53 04 00 13 <keyid> <b> <ck>` (the second byte `<b>` is a
don't-care — see fact 1). Two verified facts shape correct use:

1. **The second byte is not acted on — with one verified exception.** For nearly every
   key, every `0x13` frame injects exactly **one** key press regardless of the second byte
   (`00`, `01`, and `02` all inject one event); the vendor app always sends `01`, and one
   frame per intended press drives it (including the CSV Source cycle, §9.4). [verified]
   **The Acquire-menu LongMem/store-depth softkey (keyid 5) is the exception: it is
   edge-triggered on the second byte.** It advances only on a level *transition*, so a
   repeated `01` produces an edge on the first frame and then no-ops — the depth silently
   sticks at its prior value. Drive it with **alternating** `13 05 01` / `13 05 00` frames,
   polling `ACQURIE-STORE-DEPTH` until each step lands (see the `ACQURIE-STORE-DEPTH` row in
   §8). No other softkey observed to need this — probe/source/channel/delete/save all take a
   plain repeated `01`. [verified on hardware 2026-07-20, Python `_set_depth_via_keys` and
   the Rust `control::set_depth` port]
2. **The pending-key store is a single slot, not a queue.** A `0x13` frame writes
   `keyid` into a one-byte mailbox (idle sentinel = `0xFF`); the device's input loop
   consumes it on its next poll and clears it back to the sentinel. **Two key events
   issued faster than that poll can drop the first.** Space presses out (a few ms
   apart is safe). [inferred — mailbox behaviour; observed benign at the vendor
   app's cadence]

The **IN reply** is `53 03 00 93 <status> <ck>`: echo `0x93` (= `0x13 | 0x80`) plus
one **status byte that reflects the device's live menu/key state**, not the request.
Observed values include `0x01`, `0x0b`, `0x19`; an earlier note that it "echoes the
`01` state byte" was a coincidence (the status happened to be `01`). Do not rely on
the status byte to confirm which key you sent. [verified]

Internally the `keyid` is translated through a fixed table into the device's own
keypad scan code before it reaches the menu engine; **the host never needs that scan
code** — the `keyid` (the `/keyprotocol.inf` index) is the stable, USB-facing
identifier and is all you send. [inferred]

Softkey targeting: to press "the 3rd option in the current menu," open the menu with
its `MENU-*` key, confirm `CONTROL-MENUID` via a poll (§9.1), then send `FN-2`
(keyid 2). An option slot that does nothing in a given menu is simply inert (a
no-op), so a mis-aimed `FN-n` is harmless. [inferred]

### 9.3.1 External reference — the DSO5000-series key protocol (mikrocontroller.net)

The community **"Hantek_Protokoll"** article
(<https://www.mikrocontroller.net/articles/Hantek_Protokoll>) documents the key/menu protocol of the
sibling Hantek/Tekway **DSO5xxxB** family (same `'S'`/`'C'` framing). It corroborates the frame format
(`0x53`/`0x43` leader, LE16 length = framelen−3, `sum & 0xFF` checksum), the softkey codes **F0–F7 =
`0x00`–`0x07`**, Save/Recall = `0x0B`, and that `/keyprotocol.inf` (fetched via `0x43 0x10`) is the
authoritative key-name→code list whose 0-based line index is the `keyid`.

It also describes a separate `0x43 0x43` "debug" key command carrying an explicit press and release.
On the `0x43` command leader, selector `0x43`:
  - **Press:**   `43 <len_LE16> 43 <keycode>       01 <ck>`
  - **Release:** `43 <len_LE16> 43 <keycode|0x80>  01 <ck>`  ← release = keycode OR-ed with `0x80`.
  - Reply: `43 02 00 c3 <ck>` (echo `0x43 | 0x80`).

On the MSO5202D this press/release form is not needed: the plain `0x13` key event (§9.3, one inject
per frame) drives every softkey and menu control, including the CSV Source cycle (§9.4). `[external —
mikrocontroller.net]`

### 9.4 Save / export flows are pure key sequences (no save selector)

There is **no dedicated USB "save" command**. Saving a waveform, screenshot, setup,
or reference is performed entirely by pressing softkeys; the file the panel writes to
its USB stick is then pulled back over USB with the file-read selector `0x10`. The
on-device behaviour behind each softkey (observed as files that appear on the stick
and as the transient temp files) is:

| Panel action | Reached by | Device writes | Then |
|---|---|---|---|
| **CSV waveform** | Save/Recall → CSV page (menu 48) softkey, **or** Utility page 1 → Save-Wave | `/dsocsv.tmp` → `mv` to `/mnt/udisk/WaveData<n><n>.csv` | read back over `0x10` |
| **Screenshot** | Utility → Save-Wave (image mode) | renders BMP → converts BMP→GIF → copies **both** `.bmp` and `.gif` to `/mnt/udisk/…` | read back over `0x10` |
| **Setup** | Save/Recall → SETUP (menu 18) | `/dsosetup.tmp` → `mv` to destination | on-device recall / read back |
| **Reference** | Save/Recall → REF (menu 19) | `/ref.dat.tmp` → `mv`; refs live under `/param/sav/` (`refa`,`refa.dat`,…) | read back over `0x10` |

Reproducing a save from the host is therefore: **(1)** press `MENU-SR` (keyid 11);
**(2)** drive the CSV softkeys; **(3)** wait for the write; **(4)**
`read_file("/mnt/udisk/WaveData<n><n>.csv")` over `0x10`.

**Save→CSV softkey map `[verified 2026-07-10` by screenshotting each menu (`0x20`)]:**
the bezel softkeys map top-to-bottom to **`FN-1 … FN-6` (keyids 1–6)**, not `FN-0`.

| Menu | keyid 1 | keyid 2 | keyid 3 | keyid 4 | keyid 5 | keyid 6 |
|---|---|---|---|---|---|---|
| **S/R** (menu 47) | Ref | SetUp | **CSV** | — | — | — |
| **CSV** (menu 48) | **Source** (cycles CH1→CH2→**LA**) | **Save** | Recall | **delete ⚠** | FileList | Back |

So a CSV save is **`key 11 → keyid 3 → (keyid 1 ×N to pick Source) → keyid 2`**. `keyid 4`
is **delete** — never issue it blind (it erases card files). The **Source** selector
means a save can export **CH1, CH2, or the LA pod** — the LA path is a way to get the
16 digital channels out as a file (the live `02 01 05` read being unusable).

**The Source selector is a menu-only control — it is NOT in the `0x01` settings blob, so
there is no wire command to read or set it `[verified 2026-07-11]`.** On the CSV page it is
`keyid 1`; each `0x13` frame advances it exactly one step, wrapping **CH1→CH2→LA→CH1**. This
works in **any run-state** (running, stopped, single-seq) and **regardless of which channels are
enabled** `[verified 2026-07-14]`. Cycling changes no settings byte and there is no polled value
for its position (a host that must track it reads the `0x20` framebuffer — the selected radio is
highlighted — which is a tooling concern, not the protocol).

**The save needs the SD card mounted at `/mnt/udisk`.** `[verified 2026-07-10]` With no
card (`df /mnt/udisk` → `ubi0:rootfs`), pressing Save is a **silent no-op** — **no
`/dsocsv.tmp` is written** and no file appears (the save aborts at the USB-disk check
*before* creating the temp). So the internal temp is **not** a card-free path; with a
card it exists only transiently (created → `mv`'d in ms), so the read target is the
final `/mnt/udisk/WaveData<n><n>.csv`. `<n><n>` is a running sequence number.

Exported-file details cross-referenced elsewhere: a front-panel CSV/Ref record is
**4064 samples** (20.32 div), distinct from the 3840/3200-sample USB screen block;
saved setup/reference files are wrapped with GPG (a fixed passphrase is used for the
saved-file container, separate from the firmware-update passphrase); the CSV's second
column is **volts** (see the calibration section). This is the *only* route to
deep-memory (40K/512K/1M) records — they are never served over USB by the `0x02`
acquire path. [verified]

---

## 10. Host-control recipes

Practical, tested procedures for driving the scope from a host, built entirely on the
`0x53` data channel (Section 4) plus the settings datasheet (Section 8). Each recipe is a
sequence of frames; checksums follow the Section 3 rule (or send `0x66`).

Throughout, remember the **transport quirk** (Section 2): the device only answers if a
bulk IN read is **already pending** when you write the OUT frame — post the read first,
then write.

### 10.1 Read-modify-write a single setting

The whole instrument state is one **213-byte block** (Section 8). To change any field:

1. **Poll** the current block: `OUT 53 02 00 01 56` → `IN 53 d7 00 81 <213 bytes> ck`.
   The 213 parameter bytes start **immediately after the `0x81` echo** (no subtype).
2. **Modify** the target field's bytes in place, using the datasheet offsets in
   Section 8 (`blk` = index within the 213-byte block). Multi-byte numeric fields are
   little-endian; positions / levels are signed 1/25-div; time fields are int64
   picoseconds.
3. **Write** the block back: `OUT 53 d7 00 11 <213 bytes> ck` → `IN 53 03 00 91 <status> ck`.
   `status = 0x00` on success, `0xFF` on failure. [verified]

```
# example acknowledgement of a good write
OUT  53 d7 00 11 01 0a 00 … (213 bytes) … ck
IN   53 03 00 91 00 e7
```

Always **read-modify-write the whole block** — never synthesise a block from scratch, and
preserve every byte you are not intentionally changing. A few fields are read-only
constants (holdoff min/max, max-contrast/brightness) and writing them is ignored. Note
that some logical actions fan out: writing `TRIG-MODE` updates both
`TRIG-SWAP-CH1-MODE` and `TRIG-SWAP-CH2-MODE` in the same block. [verified]

### 10.2 Set the timebase (and pick it for a target bit rate)

Time/div is governed by **two** fields (Section 8):

- **`HORIZ-WIN-TB`** — the front-panel knob index, `0..31`, mapping to the 2-4-8 ns…s
  ladder (index 0 = 2 ns/div, larger = slower).
- **`HORIZ-TB`** — the *acquisition* timebase index, which **cannot go faster than index
  6 (200 ns/div)**.

To set a timebase to knob index `idx`, write **`HORIZ-WIN-TB = idx`** and
**`HORIZ-TB = max(idx, 6)`** in the same block, then `0x11`. [verified] For serial-decode
work, choose `idx` so the message spans the 3840-sample screen at **≈15–25 samples per
bit** (200 samples/div ⇒ pick div/bit accordingly).

### 10.3 Press a front-panel key

`OUT 53 04 00 13 <keyid> <state> ck` → `IN 53 03 00 93 <status> ck` (echo `0x93`). The
**`keyid`** is the 0-based index into `/keyprotocol.inf` (Section 9); the **`state` byte
is ignored** — every `0x13` frame is exactly one key press regardless of it (send `01` by
convention). The reply's `status` byte is the **live menu/key status** (values like
`0x01` / `0x0b` / `0x19`), *not* an echo of the state byte. [verified]

Useful keyids: **17 = AUTOSET, 19 = RUN/STOP, 21 = DEFAULT-SETUP, 46 = TRIG-50%,
47 = FORCE**. [verified]

```
OUT  53 04 00 13 11 01 7c        # keyid 17 = AUTOSET
IN   53 03 00 93 <status> <ck>
```

The key mailbox is **single-slot** (consumed when the GUI polls it): if you inject keys
faster than the scope's key-scan loop drains them, one can be dropped. **Space presses
out** (a few tens of ms) and, for toggles, verify the effect (next recipe). [verified]

### 10.4 Run / Stop (press-until-observed)

RUN/STOP is a **toggle** (keyid 19); there is no absolute set. To reach a *known* state,
press then confirm via `TRIG-STATE` in the settings poll, and repeat if needed:

```
loop:
    poll 0x01;  read TRIG-STATE     # 0 = STOP, 3 = triggered/RUN (2 = auto-searching)
    if already in desired state: done
    press key 19 (0x13);  wait ~50 ms
    poll again; if still wrong, press again        # press-until-observed
```

This idempotent loop absorbs a dropped key (10.3) and the toggle ambiguity. Do **not**
assume a single press landed. [verified]

### 10.5 Grab a screenshot (`0x20` → RGB565 → PNG)

`OUT 53 02 00 20 75` streams the LCD framebuffer with echo `0xa0`. There is **no size
frame**: you get a run of `a0 01` content frames (each ≤ 10208 bytes) totalling **exactly
768000 bytes**, then a single `a0 02 <img_sum8>` end marker whose payload byte is the
8-bit sum of all 768000 pixel bytes. [verified]

Decode: **800 × 480, 16-bit RGB565, little-endian, row-major**. Per 16-bit pixel
`R = (px >> 11) & 0x1F`, `G = (px >> 5) & 0x3F`, `B = px & 0x1F` (channel order
[inferred] — assumed MSB=R); scale each to 8-bit and write a PNG. `800 × 480 × 2 =
768000` confirms the layout. [verified]

```
OUT  53 02 00 20 75
IN   53 e3 27 a0 01 <10208 pixel bytes> ck      # first of 76 content frames (75×10208 + 2400)
     … 75 more …
IN   53 04 00 a0 02 <img_sum8> ck               # end; verify sum8 over the 768000 bytes
```

### 10.6 Synchronized 2-channel capture (STOP-then-read)

CH1 and CH2 are **separate acquires ~100 ms apart** if you read them while the scope is
free-running, so their phase relationship is lost. To capture both channels from **one
frozen simultaneous acquisition** (required for any 2-wire decode):

1. **STOP** the scope (recipe 10.4) so the on-screen record is frozen.
2. Read **CH1**: `OUT 53 04 00 02 01 00 ck` → size/data/end (echo `0x82`).
3. Read **CH2**: `OUT 53 04 00 02 01 01 ck` → size/data/end.
4. **RUN** again if you want live update.

Because the record is frozen, steps 2 and 3 return the **same simultaneous acquisition** —
clock and data edges line up. [verified] Always **read the sample count from the SIZE
frame** (`82 00 <src> <count_LE24>`, count = data-byte count; analog = 1 B/sample so
count = samples; typically 3840, sometimes 3200 when a soft-menu is open) — **never
hard-code 3840**. Samples are **signed int8**, screen-centre `0x00`, rails `0x7F`/`0x81`,
trigger column `0xFF`; convert with `y_div = (int8(byte) − 16) / 25` and
`volts ≈ y_div × Vdiv` (counts→volts scale = `Vdiv/25`, ground-truth-matched this
session). [verified]

### 10.7 Deep capture — front-panel Save-CSV, then pull the file over USB

There is **no host command for deep memory** (`0x53/0x02` and the `0x43` debug taps only
ever return the ≤ 3840-sample screen record — confirmed against the vendor app's own USB
traffic, whose largest single transfer is the 3847-byte screen block). Deep records live
only in the on-instrument **Save-waveform** flow. To retrieve a deep capture on a host
*without* a USB stick:

1. Set the desired memory depth (`ACQURIE-STORE-DEPTH`, Section 8), then **arm SINGLE**
   (key 18, recipe 10.3). Single-sequence captures **one full-depth, trigger-aligned
   record** into acquisition memory and lands in STOP — cleaner than a plain Run→Stop
   freeze (which grabs whatever was mid-flight). In **Normal** trigger mode a mis-set
   level leaves SINGLE armed forever, so **Force-Trig** (key 47) if `TRIG-STATE` has not
   reached STOP after a short wait. `[inferred]` (SINGLE + Force are each `[verified]`;
   their use as the deep-capture trigger is the natural composition.) Note SINGLE only
   changes *what is captured* — it does **not** open a deep USB stream; the readout is
   still the CSV below.
2. Drive the **Save → CSV** front-panel sequence with key presses (recipe 10.3;
   the Save/Storage menu is action-only — it writes the CSV internally, no dedicated USB
   selector). The scope writes `/mnt/udisk/WaveData<N>.csv`.
3. **Pull the file over USB** with a file read:
   `OUT 53 <len> 10 00 2f 6d 6e 74 2f 75 64 69 73 6b 2f 57 61 76 65 44 61 74 61 …csv 66`
   (`"/mnt/udisk/WaveData….csv"`, `0x66` wildcard checksum) → `90 01` content frames +
   `90 02 <sum8>` end. [verified]

A **512K-point** CSV is ≈ **7.7 MB** and reads back in **≈ 10 s** at the observed
**~800 KB/s** USB file-read rate. [verified] CSV format: header lines then rows
`<time_seconds>,<volts>` (`%0.5E,%0.3f`); the value column is **volts** (the
`#voltbase=…(mv/100)` header label is a misnomer — the stored magnitude is µV/div, so
`V/div = voltbase/1e6`). The on-instrument save record length is **4064 samples** at
screen depth (distinct from the 3840 USB screen block), 40064 / 400064 for deep saves; the
per-sample `dt` is depth-driven (4K → 20 ns/50 MSa/s; 40K & 512K → 5 ns/200 MSa/s), **not**
the constant `#timebase` header tag. [verified]

### 10.8 Serial-bus decode (UART / SPI / I²C) — constraints

The scope has no built-in bus decoder, but a host can reconstruct UART / SPI / I²C from
captured analog traces (implemented in `scripts/serial_decode.py` +
`scripts/mso5202d_decode.py`; test signals from `scripts/esp_combo_gen/`, an ESP32 that
streams a `0x00..0xFF` ramp). Verified end-to-end this session. The constraints all come
from the transport, not the decoder:

- **Only 2 signals per capture.** CH1/CH2 are the only usable inputs (LA-over-USB is a
  dead firmware path — Section 6). So **UART = 1 line; SPI = SCLK + one data line**
  (MOSI *or* MISO, no full-duplex, no CS channel — bytes re-frame on an idle-clock gap);
  **I²C = SDA + SCL**. [verified]
- **SPI missed-edge vs. real-gap — resolved from the analog clock.** Without CS, a gap of a
  few bit-periods between clock edges is ambiguous: it can be a bandwidth-dropped clock pulse
  (reconstruct the edge) OR a real inter-word transaction idle (start a new byte). Timing alone
  can't tell them apart — no edge-triggered logic analyzer can. But a passive scope capture has
  the raw clock waveform: inspect the MIDDLE of the gap — **flat at idle ⇒ real gap (reframe);
  a sub-threshold swing ⇒ missed pulse (reconstruct)**. This fixes low-frequency continuous
  streams (10 kHz: 0.25 → 0.94 ramp on hardware) with no regression on fast lines. [verified 2026-07-15]
- **Short messages only** — the record is ≤ 3840 samples (no deep memory over USB), so a
  message must fit one screen. **Set the timebase** for the bit rate (recipe 10.2,
  ≈ 15–25 samples/bit). [verified]
- **Threshold** the analog trace to logic: unwrap bytes → divisions
  (`y_div = (int8(byte) − 16)/25`), then Schmitt-trigger against the signal's **local
  envelope** — a sliding max/min over ~1.5 clock periods sets a per-sample midpoint, so a
  fast line whose low level droops during active bursts (AC coupling / limited bandwidth)
  still resets every cycle where a single global threshold would drop edges. Use the
  **STOP-then-read** synchronized capture (recipe 10.6) so the clock and data channels come
  from one frozen acquisition. [verified]
- **Some frozen acquisitions come back glitched** (more so at slow timebases) — re-capture;
  the decoders are deterministic. [verified]

Verified bit-rate ranges (ESP32 sources, ramp decoded byte-for-byte): UART 8N1
**9600 – 115200 baud**; SPI mode 0 MSB **10 kHz – 20 MHz SCLK** (20 MHz decoded from a
512K deep capture at ~10 samples/clock via the local-envelope threshold); I²C
**~17 – 167 kHz SCL** (source ceiling; the edge-driven decoder has no inherent limit).
[verified]

## 11. Hardware & device background (appendix)

This appendix documents the physical instrument behind the wire protocol. Facts here
were reconstructed from the device's own filesystem — every file named below is
readable over USB with the file-read selector `0x10` — and from observed device
behaviour. It is background: none of it is required to drive the scope over USB, but
it explains *why* the protocol looks the way it does (e.g. why a waveform sample is a
16-bit FPGA FIFO word, why the framebuffer is exactly 768000 bytes, why a crashed app
reappears in ~100 ms).

### 11.1 Physical memory map

The instrument is an ARM SoC (S3C2416 family, CPU 400 MHz / bus 100 MHz per
`/dso/driver/driver.log`) with two external FPGAs on separate chip-select banks:

| Physical base | Region | Role |
|---|---|---|
| `0x10000000` | nGCS2 | **Analog front-end FPGA** data window (acquisition samples) |
| `0x18000000` | nGCS3 | **Logic-analyzer FPGA** data window |
| `0x4F000000` | 516-byte SFR block | FPGA / SROM **bus-timing control** registers (set once at boot; two parallel FPGA-control register groups 0x20 apart, one per FPGA) |
| `0x58000000` | 1 MB | SoC **housekeeping/touch ADC** (not the waveform ADC) |
| `0x20000000` | nGCS4 | **DM9000 Ethernet** controller |

Correction to earlier notes: the analog/LA sample windows are `0x10000000` /
`0x18000000` — **not** `0x4F000000`. The `0x4F000000` block is only bus/SROM timing
control (the value `11811` = `0x2E23` from `/fpgabank.conf` is programmed into it at
boot; the boot log prints `fpga bank 11811`). [inferred]

### 11.2 FPGA register model (why samples are 16-bit FIFO words)

Each FPGA data window is a bank of **256 × 16-bit registers, halfword-addressed**:
register `addr` (0..0xFF) lives at `base + addr*2`. Reads/writes are 16-bit
(`ldrh`/`strh`-width). Crucially, **bulk acquisition is a single auto-advancing FIFO
port**: the acquisition block is read by hammering *one* register `addr` `count`
times, and the FPGA advances its internal sample pointer on each 16-bit read. There
is no large contiguous sample buffer to mmap — deep memory never crosses this port to
the host, which is the hardware reason the USB `0x02` path only ever returns the
on-screen block. Analog samples arrive as one byte each on the wire (the FPGA's 16-bit
word narrowed to a signed int8, §6); the LA path is genuinely 2 bytes/sample (16 D-bit
word). [inferred]

### 11.3 Device nodes & control paths

The instrument's peripherals are exposed as device nodes with these observed control
conventions (background — the USB protocol does not touch them directly):

| Node | What | Control |
|---|---|---|
| `/dev/dso-fpga` | analog FPGA window (`0x10000000`) | mmap; 16-bit regs at `base+addr*2` |
| `/dev/dso-fpga-la` | LA FPGA window (`0x18000000`) | mmap |
| `/dev/dso-iobank` | GPIO + SROM bank width | ioctl `0x6900`=out-low, `0x6901`=out-high, `0x6902`=input (arg `(bank<<8)\|pin`); `0x6903/04/05`=SROM width 8/16/32-bit |
| `/dev/adc` | SoC housekeeping/touch ADC | ioctl `0x4100`=channel-select (0..5); `read()` → ASCII decimal |
| `/dev/dso-buzzer` | piezo buzzer (PWM) | ioctl `0x6200`=beep (arg=count), `0x6201`=off; other cmd numbers = alternate tones |
| `/dev/bkl` | LCD backlight (PWM) | `write()` a 4-byte LE int: `-1`=off, `1..105`=brightness level |
| `/dev/fb0` | LCD framebuffer | mmap **768000 B = 800×480 RGB565** |

The `/dev/fb0` geometry (768000 bytes, 800×480, RGB565) is exactly the framebuffer
selector `0x20` payload — the USB "screenshot" is a copy of this LCD memory. [verified
for the 768000-byte size this session; node/ioctl details [inferred]]

### 11.4 Device identity (`/sys.inf`, `/i2c.log`)

Two files establish the concrete unit's identity; both are readable over USB (`0x10`):

`/sys.inf` (firmware identity):

```
[DST type]dst1202b
[soft version]3.2.35(180502.0)     ; app version, build date 2018-05-02
[fpga version]0x55778344           ; loaded analog-FPGA bitstream id
[start time]141                    ; power-on / boot counter
[update time]0                     ; firmware-update counter (0 = never updated over USB)
```

`/i2c.log` — the on-board identity EEPROM image (8 KB; 16-byte binary header, zero
padding, then an ASCII identity block at offset `0x1C00`):

```
[serial number]T 1G/112 030641
[operation time]2018-05-18 14:44:37
[operator]hantek
[pcb]102  [lcd]3  [front]1
[usb]0 [touch]0 [net]0 [iso]0 [sd]0 [vei]0 [dds]0 [key]0 [genamp]0   ; option flags (0 = absent)
[buf]2                             ; memory / buffer size class
[bw]200                            ; ANALOG BANDWIDTH = 200 MHz  ← authoritative MSO5202D tag
```

`[bw]200` is the authoritative in-EEPROM bandwidth stamp confirming this is the
**200 MHz MSO5202D** (vs. the 60/100 MHz base models). The model string is carried in
two files: `/logotype` = `dst1202b` (internal model key) and `/logotype.dis` =
`Hantek_MSO5202D` (display brand). `/language.img` holds `license 6143 / locale 1 /
domain 1`. [verified — file contents]

### 11.5 Calibration & timing files (for completeness)

Also on the filesystem and readable over `0x10`: `/chk_base_volt` (4-point gain-cal
reference codes `[8mv]195 [20mv]387 [400mv]405 [2000mv]2006`); `/param/sav/chk1kb_*`
(8664-byte per-lane ADC linearization table); `/tdc.log`, `/tdc_edge125M`,
`/tdc_pulse125M` (TDC fine-timing histogram and 50-entry picosecond offset tables per
trigger engine); `/cur_acq.type` (4-byte LE int persisting the acquire type). These
feed the counts→volts and sub-sample-timing math documented in the calibration
section. [verified — file contents; per-lane semantics [inferred]]

### 11.6 Supervisor / watchdog / firmware-update model

Two behaviours visible from the host are explained here:

- **A crashed app auto-respawns in ~100 ms.** The scope-engine process is a *guarded
  child* of a supervisor that monitors it via a heartbeat over a local UNIX socket
  and relaunches it if the heartbeat stops (watch interval ≈ 100 ms). A hard engine
  crash therefore causes a brief USB-link drop and reconnect — this is **normal**, not
  a protocol fault, and is why a robust host driver should tolerate transient
  disconnects (the repo's reconnect feature exists for exactly this). Independently, a
  hardware watchdog (`/dev/watchdog`) resets the SoC if userspace wedges entirely.
  [inferred]

- **Firmware update is file-triggered, not a USB command.** Dropping an update package
  (`emerg.do` / `force.up` / `system*.up` / `dst1000_4000.up`) onto the USB stick, or
  a staged `/dso_update.exe`, causes the supervisor to decrypt (GPG) and unpack it into
  the root filesystem and reboot. There is **no over-the-wire firmware-flash selector**;
  the `0x53`/`0x43` protocol never carries firmware. [inferred]

Shipped-firmware note (observed in the filesystem): the unit boots with a fixed dev IP
`192.168.1.127`, an unauthenticated root `telnetd`, and an empty root password — a
wide-open local debug/backdoor path, unrelated to the USB protocol. [verified — config
files]

---

## Appendix A — full `/protocol.inf` (settings parameter list; number = byte width)

The scope self-describes its 213-byte settings block via this file (read over USB with selector `0x10`). Each `[NAME] n` is a field and its byte width, in blob order. `[TOTAL] 213` = sum of widths (§8).

```
[TOTAL]  213
[START]
[VERT-CH1-DISP]            1
[VERT-CH1-VB]              1
[VERT-CH1-COUP]            1
[VERT-CH1-20MHZ]           1
[VERT-CH1-FINE]            1
[VERT-CH1-PROBE]           1
[VERT-CH1-RPHASE]          1
[VERT-CH1-CNT-FINE]        1
[VERT-CH1-POS]             2
[VERT-CH2-DISP]            1
[VERT-CH2-VB]              1
[VERT-CH2-COUP]            1
[VERT-CH2-20MHZ]           1
[VERT-CH2-FINE]            1
[VERT-CH2-PROBE]           1
[VERT-CH2-RPHASE]          1
[VERT-CH2-CNT-FINE]        1
[VERT-CH2-POS]             2
[TRIG-STATE]               1
[TRIG-TYPE]                1
[TRIG-SRC]                 1
[TRIG-MODE]                1
[TRIG-COUP]                1
[TRIG-VPOS]                2
[TRIG-FREQUENCY]           8
[TRIG-HOLDTIME-MIN]        8
[TRIG-HOLDTIME-MAX]        8
[TRIG-HOLDTIME]            8
[TRIG-EDGE-SLOPE]          1
[TRIG-VIDEO-NEG]           1
[TRIG-VIDEO-PAL]           1
[TRIG-VIDEO-SYN]           1
[TRIG-VIDEO-LINE]          2
[TRIG-PULSE-NEG]           1
[TRIG-PULSE-WHEN]          1
[TRIG-PULSE-TIME]          8
[TRIG-SLOPE-SET]           1
[TRIG-SLOPE-WIN]           1
[TRIG-SLOPE-WHEN]          1
[TRIG-SLOPE-V1]            2
[TRIG-SLOPE-V2]            2
[TRIG-SLOPE-TIME]          8
[TRIG-SWAP-CH1-TYPE]       1
[TRIG-SWAP-CH1-MODE]       1
[TRIG-SWAP-CH1-COUP]       1
[TRIG-SWAP-CH1-EDGE-SLOPE] 1
[TRIG-SWAP-CH1-VIDEO-NEG]  1
[TRIG-SWAP-CH1-VIDEO-PAL]  1
[TRIG-SWAP-CH1-VIDEO-SYN]  1
[TRIG-SWAP-CH1-VIDEO-LINE] 2
[TRIG-SWAP-CH1-PULSE-NEG]  1
[TRIG-SWAP-CH1-PULSE-WHEN] 1
[TRIG-SWAP-CH1-PULSE-TIME] 8
[TRIG-SWAP-CH1-OVERTIME-NEG]        1
[TRIG-SWAP-CH1-OVERTIME-TIME]       8
[TRIG-SWAP-CH2-TYPE]       1
[TRIG-SWAP-CH2-MODE]       1
[TRIG-SWAP-CH2-COUP]       1
[TRIG-SWAP-CH2-EDGE-SLOPE] 1
[TRIG-SWAP-CH2-VIDEO-NEG]  1
[TRIG-SWAP-CH2-VIDEO-PAL]  1
[TRIG-SWAP-CH2-VIDEO-SYN]  1
[TRIG-SWAP-CH2-VIDEO-LINE] 2
[TRIG-SWAP-CH2-PULSE-NEG]  1
[TRIG-SWAP-CH2-PULSE-WHEN] 1
[TRIG-SWAP-CH2-PULSE-TIME] 8
[TRIG-SWAP-CH2-OVERTIME-NEG]        1
[TRIG-SWAP-CH2-OVERTIME-TIME]       8
[TRIG-OVERTIME-NEG]        1
[TRIG-OVERTIME-TIME]       8
[HORIZ-TB]                 1
[HORIZ-WIN-TB]             1
[HORIZ-WIN-STATE]          1
[HORIZ-TRIGTIME]           8
[MATH-DISP]                1
[MATH-MODE]                1
[MATH-FFT-SRC]             1
[MATH-FFT-WIN]             1
[MATH-FFT-FACTOR]          1
[MATH-FFT-DB]              1
[DISPLAY-MODE]             1
[DISPLAY-PERSIST]          1
[DISPLAY-FORMAT]           1
[DISPLAY-CONTRAST]         1
[DISPLAY-MAXCONTRAST]      1
[DISPLAY-GRID-KIND]        1
[DISPLAY-GRID-BRIGHT]      1
[DISPLAY-MAXGRID-BRIGHT]   1
[ACQURIE-MODE]             1
[ACQURIE-AVG-CNT]          1
[ACQURIE-TYPE]             1
[ACQURIE-STORE-DEPTH]      1
[MEASURE-ITEM1-SRC]        1
[MEASURE-ITEM1]            1
[MEASURE-ITEM2-SRC]        1
[MEASURE-ITEM2]            1
[MEASURE-ITEM3-SRC]        1
[MEASURE-ITEM3]            1
[MEASURE-ITEM4-SRC]        1
[MEASURE-ITEM4]            1
[MEASURE-ITEM5-SRC]        1
[MEASURE-ITEM5]            1
[MEASURE-ITEM6-SRC]        1
[MEASURE-ITEM6]            1
[MEASURE-ITEM7-SRC]        1
[MEASURE-ITEM7]            1
[MEASURE-ITEM8-SRC]        1
[MEASURE-ITEM8]            1
[CONTROL-TYPE]             1
[CONTROL-MENUID]           1
[CONTROL-DISP-MENU]        1
[LA-SWI]                   1
[LA-CHANNEL-STATE]         2
[LA-CURRENT-CHANNEL]       1
[LA-D7-D0-THRESHOLD-TYPE]	1
[LA-D15-D8-THRESHOLD-TYPE]	1
[LA-D7-D0-USER-THRESHOLD-VOLT]	2
[LA-D15-D8-USER-THRESHOLD-VOLT]	2
[END]
```

## Appendix B — full `/keyprotocol.inf` (front-panel key list)

The 0-indexed position of each key entry in this file is its `keyid` for the `0x13` key-event selector (§4, §9).

```
[TOTAL]  49
[START]
[FN-0-KEY] 1
[FN-1-KEY] 1
[FN-2-KEY] 1
[FN-3-KEY] 1
[FN-4-KEY] 1
[FN-5-KEY] 1
[FN-6-KEY] 1
[FN-7-KEY] 1
[FN-MLEFT-KEY] 1
[FN-MRIGHT-KEY] 1
[FN-MZERO-KEY] 1
[MENU-SR-KEY] 1
[MENU-MEASURE-KEY] 1
[MENU-ACQUIRE-KEY] 1
[MENU-UTILITY-KEY] 1
[MENU-CURSOR-KEY] 1
[MENU-DISPLAY-KEY] 1
[CT-AUTOSET-KEY]  1
[CT-SINGLESEQ-KEY] 1
[CT-RS-KEY] 1
[CT-HELP-KEY] 1
[CT-DS-KEY] 1
[CT-STU-KEY] 1
[VT-MATH-MENU-KEY] 1
[VT-CH1-MENU-KEY] 1
[VT-CH1-PSUB-KEY] 1
[VT-CH1-PADD-KEY] 1
[VT-CH1-PZERO-KEY] 1
[VT-CH1-VBSUB-KEY] 1
[VT-CH1-VBADD-KEY] 1
[VT-CH2-MENU-KEY] 1
[VT-CH2-PSUB-KEY] 1
[VT-CH2-PADD-KEY] 1
[VT-CH2-PZERO-KEY] 1
[VT-CH2-VBSUB-KEY] 1
[VT-CH2-VBADD-KEY] 1
[HZ-MENU-KEY] 1
[HZ-PSUB-KEY] 1
[HZ-PADD-KEY] 1
[HZ-PZERO-KEY] 1
[HZ-TBSUB-KEY] 1
[HZ-TBADD-KEY] 1
[TG-MENU-KEY] 1
[TG-PSUB-KEY] 1
[TG-PADD-KEY] 1
[TG-PZERO-KEY] 1
[TG-PHALF-KEY] 1
[TG-FORCE-KEY] 1
[TG-PROBECHECK-KEY] 1
[END]
```

## Appendix C — annotated example frames

One real OUT+IN pair per selector, byte-by-byte. Framing recap:
`leader | len_LE16 | payload | checksum`, where **`len = (bytes after the len field)`
= payload + 1 (the checksum byte)**, and **`checksum = (Σ all bytes except the
checksum) & 0xFF`**. The IN payload's first byte is the echo = `selector | 0x80`. The
special checksum value `0x66` is a **wildcard** the validator always accepts.

### C.1 Leader `0x53` — data channel

```
--- 0x00  connect / ping ---
OUT  53 02 00 00 55
     53          leader 'S'
        02 00    len = 2  (payload 0x00 + checksum)
              00 selector 0x00
                 55  checksum = (53+02+00+00)&0xFF
IN   53 02 00 80 d5
     80          echo = 0x00|0x80 ; empty ack, no data

--- 0x01  poll settings ---
OUT  53 02 00 01 56
              01 selector 0x01 (no args)
IN   53 D7 00 81 <213 param bytes> <ck>
     D7 00       len = 0x00D7 = 215 (echo + 213 params + checksum)
        81       echo = 0x01|0x80
        <213 B>  the settings blob (§8) — param[0] is at frame byte offset 4

--- 0x02  acquire, CH1 ---
OUT  53 04 00 02 01 00 5a
              02 selector 0x02
                 01  sub = 1  (acquire; sub=0 would be a latch, not a read)
                    00  channel 0 = CH1  (1=CH2, 2=Math, 5=LA)
                       5a checksum
IN   (frame 1, SIZE)  53 07 00 82 00 00 00 0f 00 eb
     82            echo = 0x02|0x80
        00         subtype 0x00 = SIZE
           00      src = 0 (CH1; CH2=1, LA=3)
              00 0f 00   count = 24-bit LE = 0x000F00 = 3840  (DATA-byte count;
                         for LA it is 2×samples)
IN   (frame 2, DATA)  53 04 0f 82 01 00 <3840 signed-int8 samples> <ck>
     04 0f         len = 0x0F04 = 3844 (echo+sub+ch + 3840 + ck)
        82 01 00   echo / subtype 0x01 = DATA / channel 0
IN   (frame 3, END)   53 04 00 82 02 00 db
        82 02 00   subtype 0x02 = END-marker (MUST be consumed or the stream desyncs)
        (subtype 0x03 = "no fresh data", e.g. LA idle: 53 04 00 82 03 03 df)

--- 0x10  file read ("/protocol.inf") ---
OUT  53 10 00 10 00 2f 70 72 6f 74 6f 63 6f 6c 2e 69 6e 66 66
     10 00         len = 16
        10         selector 0x10
           00      reserved/subtype byte
              2f..66  path ASCII "/protocol.inf" (13 bytes)
                       66  checksum = 0x66 WILDCARD (0x10 always uses it)
IN   (content) 53 <len> 90 01 <up to 10208 file bytes> <ck>   ; repeated N times
     90 01         echo / subtype 0x01 = content chunk (cap 10208 B each)
IN   (end)     53 04 00 90 02 61 4a
        90 02      subtype 0x02 = END
              61   sum8 = 8-bit sum of ALL file bytes (0x61 for /help.db, verified)
     (no SIZE frame is ever sent for 0x10)

--- 0x11  write settings ---
OUT  53 D7 00 11 <213 param bytes> <ck>
        11         selector 0x11 ; payload = the full 213-byte blob (read-modify-write)
IN   53 03 00 91 00 e7
     91            echo = 0x11|0x80
        00         status 0x00 = OK   (0xFF = rejected: 53 03 00 91 ff e6)

--- 0x12  run / acquire latch ---
OUT  53 04 00 12 01 01 6b
        12 01 01   selector / sub=1 (run-latch) / val=1
IN   53 04 00 92 01 01 eb
     92 01 01      echo / sub / val echoed
     (sub=0 path: OUT 53 04 00 12 00 00 69  -> IN 53 03 00 92 00 e8, acq-mode;
      neither sub is a STOP or panel-lock)

--- 0x13  key event (Run/Stop = keyid 19 = 0x13) ---
OUT  53 04 00 13 13 01 7e
        13         selector 0x13
           13      keyid = 19 (CT-RS-KEY)
              01   state byte — TRANSMITTED BUT IGNORED (00 gives the same one press)
IN   53 03 00 93 01 ea
     93            echo = 0x13|0x80
        01         live menu/key status byte (NOT an echo of the state byte)

--- 0x20  framebuffer (screenshot) ---
OUT  53 02 00 20 75
              20   selector 0x20 (no args)
IN   (data)  53 e3 27 a0 01 <10208 pixel bytes> <ck>   ; 75 full chunks
     e3 27         len = 0x27E3 = 10211 (echo+sub + 10208 + ck)
     a0 01         echo = 0x20|0x80 / subtype 0x01 = data
IN   (last data) 53 ... a0 01 <2400 pixel bytes> <ck>  ; final short chunk
IN   (end)   53 04 00 a0 02 47 40
     a0 02         subtype 0x02 = END
           47      sum8 = 8-bit sum of all 768000 pixel bytes
     Total pixels = 768000 = 800×480×2 (RGB565). NO size frame.

--- 0x14 / 0x21  descriptor write / read (purpose unknown) ---
OUT  53 <len> 14 <id_lo> <id_hi> <bytes>     ; descriptor WRITE, no IN reply
OUT  53 02 00 21 76  ->  IN 53 <len> a1 <descriptor bytes> <ck>
     a1            echo = 0x21|0x80 ; default id16 observed = 0x07D9 (2009). [gap: contents]
```

### C.2 Leader `0x43` — private command / shell channel

Identical framing, a **separate** selector map, and a superset of capabilities the
vendor app never uses. Present for completeness; treat as privileged.

```
--- 0x43 / 0x10  file read (same as 0x53/0x10) ---
OUT  43 10 00 10 00 2f 70 72 6f ... 66     ; echo 0x90, wildcard checksum

--- 0x43 / 0x11  shell exec (runs as root) ---
OUT  43 06 00 11 6c 73 20 2f 88
        11         selector 0x11
           6c 73 20 2f   ASCII "ls /"   ; executed by a root shell
IN   (command output, framed)               ; [inferred] echo 0x91

--- 0x43 / 0x02, 0x03  FPGA region dumps (config/cal, not samples) ---
OUT  43 02 00 02 <ck>  -> region-1 (~5072 B, analog run/set block)
OUT  43 02 00 03 <ck>  -> region-2 (~8664 B, ADC linearization table)

--- 0x43 / 0x44  beep ---
OUT  43 03 00 44 <arg> <ck>  -> IN echo 0xC4

--- 0x43 / 0x7f  commit / apply settings (NOT a reboot) ---
OUT  43 02 00 7f c4
              7f   persist-if-dirty + re-apply-all
IN   43 02 00 ff 44
     ff            ack (echo)
```

---

## Appendix D — verified / inferred / gap ledger

Compact status of every major claim. **V** = seen on the wire / confirmed on
hardware; **I** = inferred from cross-referenced captures, on-device files, or the
manual; **G** = unknown / open.

### Transport, framing, selectors

| Claim | Status |
|---|---|
| VID:PID `049f:505a`, bulk OUT `0x02` / IN `0x81`, 512-B packets | V |
| Frame `leader\|len_LE16\|payload\|checksum`; `checksum = Σ&0xFF`; `0x66` = wildcard accepted on any frame | V |
| Two leaders: `0x53` data channel, `0x43` private command/shell channel | V |
| IN echo is always `selector\|0x80` | V |
| `0x00` connect/ping (empty ack) is a distinct selector from `0x01` poll | V |
| `0x01` returns the 213-byte settings blob; `0x11` writes it (status `00`/`FF`) | V |
| `0x02 01 <ch>` acquire; channel map 0=CH1,1=CH2,2=Math,5=LA | V (Math path unused by vendor) |
| SIZE frame `82 00 <src> <count_LE24>`; count = DATA-byte count; LA count = 2×samples; size `src` = 0/1/3 | V |
| Acquire subtypes 00=SIZE, 01=DATA, 02=END, **03=no-data** | V |
| Record length is variable (read it from SIZE); 3840↔3200 tracks `CONTROL-DISP-MENU` | V (3840/3200 V; menu-driven toggle I) |
| `0x10` file read: content(`90 01`, 10208-B cap) + end(`90 02 <sum8>`), no SIZE, wildcard `0x66` ck | V |
| `0x10` end sum8 = 8-bit sum of all file bytes (`/help.db` → 0x61) | V |
| `0x12 <sub> <val>`: sub=1 run-latch, sub=0 acq-mode — NOT stop/panel-lock | V |
| `0x13` key event: `state` byte ignored; reply status = live menu state; single-slot mailbox | V (mailbox drop-risk I) |
| `0x20` framebuffer = exactly 768000 B (800×480 RGB565), no SIZE frame, END carries pixel sum8 | V |
| `0x43 0x7f` = commit/apply settings, ack `0xff` — not a reboot | I |
| `0x14` / `0x21` descriptor write/read (id16 default 2009) — purpose | **G** |

### Waveform & calibration

| Claim | Status |
|---|---|
| Analog sample = two's-complement **signed int8**; centre `0x00`, rails `0x7F`/`0x81`, trigger column `0xFF` | V |
| Larger signed value = higher on screen (no unsigned "wrap/hash"); `0x80` never occurs | V |
| `y_div = (int8(byte) − 16) / 25`; 25 counts/div; 200 samples/div | V (scale V; the +16 baseline term I) |
| counts→volts = `(raw − zero) × Vdiv/25` (Vdiv/25 V per count), matches CSV ground truth | V |
| LA = 2 bytes/sample, bit N = D(N); `02 01 05` is issued by the vendor app | V (content usability G) |
| Off-screen bimodal `0x0A`/`0xF2` blocks (not the ±127 rails) — origin | **G** |
| `+16`-count baseline: fixed bias vs. POS-zero artifact | **G** |
| No deep-memory readout over USB (40K/512K/1M) — only via front-panel save→CSV→stick | V |

### Settings blob, menus, keys

| Claim | Status |
|---|---|
| `/protocol.inf` Σwidth = 213; `wire_offset = raw + 4`; all fields little-endian | V |
| Enum tables (trigger/display/acquire/math/measure/LA) as in the datasheet section | V (most) / I (measure 12,14,15,16,18) |
| `MEASURE-ITEMn` types **20–31** (and 12/14/15/16/18) — never seen on the wire | **G** (label-derived) |
| `MATH-FFT-WIN` = 0/1/2 only (Hanning/Flattop/Rect); codes 3/4 (Bartlett/Blackman) | 0/1/2 V; 3/4 **G** (likely nonexistent) |
| `TRIG-VPOS` scales with V/div (reaches 31000 at 2 mV/div); ±200 is only the fixed-V/div knob limit | V |
| EXT / EXT-5 / AC-line trigger level in volts (the /25-div formula assumes a CH source) | **G** |
| `TRIG-STATE` enum 0–6; **4 (Scan)** and **6 (re-arm)** never captured | 0/1/2/3/5 V; 4/6 **G** |
| Read-only/const fields: `TRIG-HOLDTIME-MIN/-MAX`, `DISPLAY-MAXCONTRAST/-MAXGRID-BRIGHT` | V |
| `HORIZ-WIN-STATE`, `CONTROL-TYPE` always 0 — function | **G** |
| `VERT-CHx-CNT-FINE` fine-gain vernier magnitude/units | **G** |
| `CONTROL-MENUID` map as in §9.1; candidate ids 58/69 and the unmapped id set | mapped ids V; 58/69 + rest **G** |
| `/keyprotocol.inf` 49-key list, keyid = 0-indexed position | V |
| Save/export are pure `0x13` key sequences; no save selector; which `FN-n` = "Save" per page | flow V; ordinal & filename numbering **G** |

### Hardware background (appendix)

| Claim | Status |
|---|---|
| Analog FPGA `0x10000000`, LA FPGA `0x18000000`, ctrl SFRs `0x4F000000`, SoC ADC `0x58000000`, DM9000 `0x20000000` | I |
| FPGA regs = 256×16-bit halfword-addressed; bulk read = one auto-advancing FIFO port | I |
| `/dev/fb0` = 768000 B (800×480 RGB565) = the `0x20` payload | V (size) / I (node) |
| Identity: `/sys.inf` (`dst1202b`, sw `3.2.35`, fpga `0x55778344`), `/i2c.log` `[bw]200`, S/N | V (file contents) |
| Supervisor heartbeat (~100 ms) auto-respawns a crashed engine → expected transient USB drops | I |
| Firmware update is file-triggered (GPG package on the stick) — no USB flash selector | I |
| `0x4F000000` SFR field semantics; exact FPGA register/address assignments | **G** |
