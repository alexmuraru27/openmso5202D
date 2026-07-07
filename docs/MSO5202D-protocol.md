# Hantek MSO5202D — USB protocol reference

**Status: transport, framing and the core handshakes are reverse-engineered and
verified against real hardware (Linux/pyusb).** A working driver (`mso5202d.py`)
connects, reads the scope's self-description, decodes live settings, and captures
waveforms (a 1 kHz cal square wave was confirmed). This document is intended to be
**self-contained** — if all other context is lost, everything needed to re-derive
and re-implement the driver is here.

Everything below was decoded from two Wireshark/USBPcap captures of the vendor
Windows app ("Scope 2.0.0.6") driving the scope, plus live experiments on the
hardware. Captures live in `../captures/` (stripped to MSO-only traffic).

- Device: **Hantek MSO5202D**, 2ch 200 MHz MSO. Unit tested: SW `3.2.35(180502.0)`,
  HW `1020x55778344`. Part of the Hantek/Tekway/Voltcraft "DSO hack" family
  (Samsung S3C ARM + embedded Linux 3.2.35).
- This protocol is unrelated to the Hantek DSO-2xxx/52xx bulk protocol used by the
  cheap FX2-based scope adapters. It is a custom, self-describing, `'S'`-framed
  protocol carrying ASCII INI files, a binary settings blob, and 8-bit waveforms.

---

## 1. USB transport

| | |
|---|---|
| VID:PID | **`049f:505a`** (Linux labels it "CDC Subset Device / Itsy"; misleading) |
| Device class | `255` (Vendor Specific) — **not** actually CDC |
| Interface | 1 interface, class 255, 2 endpoints |
| Bulk **OUT** | **`0x02`** — host → scope commands |
| Bulk **IN** | **`0x81`** — scope → host responses/data |
| Max packet | 512 bytes (USB high-speed) |
| Firmware upload | none (the scope runs its own embedded Linux) |

On **Windows** the vendor driver `dstusb.sys` (a Cypress **EZ-USB** derivative;
it creates `\Device\Ezusb-0`, opened by the app as `\\.\Ezusb-0`) owns the device
and drives it with EZ-USB IOCTLs (bulk read/write + vendor requests). On **Linux**
the generic `cdc_subset` usbnet driver auto-binds it (creating `usb0`) purely
because `049f:505a` is that driver's built-in default ID — it is not really a
network device. See `MSO5202D-investigation.md` for the full hardware/software RE.

---

## 2. Linux connection recipe (CONFIRMED working) — and WHY each step

The scope was designed for the Windows EZ-USB driver, which owns the endpoints
from enumeration and keeps an IN transfer permanently posted. Reproducing that on
Linux via libusb/pyusb requires this exact sequence (implemented in
`mso5202d.py`):

1. **Detach the `cdc_subset` kernel driver** from interface 0.
   *Why:* on Linux `cdc_subset` claims interface 0 the moment the scope is
   plugged in. Until it is detached, `libusb_claim_interface` fails with
   `LIBUSB_ERROR_BUSY` and no bulk I/O is possible.

2. **`dev.reset()` (`libusb_reset_device`), then re-detach `cdc_subset`** if it
   re-binds after the reset.
   *Why (critical):* without the reset, the OUT write succeeds but **every IN
   read times out**. `cdc_subset` had been running a usbnet session on these
   endpoints, leaving the device-side gadget in a state where it will not answer
   the scope protocol. The USB port reset re-initialises the device's gadget to a
   clean state so it responds. This was determined empirically: identical code
   fails without `reset()` and succeeds with it. The reset re-enumerates the
   device, so `cdc_subset` may grab it again — hence the second detach.

3. **Claim interface 0**, then **`clear_halt` on both endpoints** (`0x02`, `0x81`).
   *Why:* resets the bulk data toggles to DATA0 on host and device so the first
   transfers aren't dropped due to a toggle mismatch left over from `cdc_subset`.

4. **Post the bulk IN read BEFORE writing the OUT command.**
   *Why (critical):* the device only delivers its reply when an IN transfer is
   already pending (mirroring the Windows driver, which always has an IN posted).
   With naive synchronous *write-then-read*, the reply is missed and the read
   times out. The driver runs the IN read in a background thread, sleeps ~30 ms so
   the IN URB is in flight, then does the OUT write.

5. **Use a persistent RX buffer and consume whole frames.**
   *Why:* responses span multiple 512-byte USB packets and several logical frames
   arrive back-to-back (e.g. a file read returns a content frame *and* an
   end-marker frame — see §5). If leftover bytes are discarded instead of kept,
   the next read starts mid-frame and the stream desyncs (symptoms we hit: an
   "empty" file read, and a settings blob that decoded as INI text `[`/`]`).

To run **without root**, install a udev rule granting access to `049f:505a`
(see `../70-mso5202d.rules` at the repo root → copy to `/etc/udev/rules.d/`);
otherwise run as root.
Note: running a GUI (matplotlib) under `sudo` breaks X access, so the udev rule
is the right way for the live viewer.

---

## 3. Frame format (both directions)

```
byte 0      : 0x53  ('S')  start-of-frame
byte 1..2   : length, little-endian uint16   ==  (total_frame_len - 3)
byte 3..N-2 : payload
byte N-1    : checksum = (sum of all preceding bytes) & 0xFF
```

- **Checksum** = 8-bit sum of every byte before it. Verified, e.g.
  `53 04 00 12 01 01 6b` → 0x53+04+00+12+01+01 = 0x6B ✓;  `53 02 00 01 56` → 0x56 ✓.
- **Length** = bytes[1..2] LE = `total_len − 3` (i.e. counts payload + checksum).
  Verified: settings blob 218 B → `d7 00` (0x00D7=215); protocol.inf 3620 B →
  `21 0e` (0x0E21=3617); waveform 3847 B → `04 0f` (0x0F04=3844).

> **Framing gotcha:** bytes[1..2] are the *length only*. The command **selector**
> is the first payload byte (byte 3). Do not mistake the length low-byte for an
> opcode — `53 02 …` / `53 10 …` are lengths 2 / 16, not "cmd 0x02 / 0x10".

### Payload — OUT (host → scope)
`payload = selector(1) | args…`

| selector | args | meaning |
|---|---|---|
| `0x01` | (none), or `00` | **keep-alive / poll.** Response is the settings-state blob (§6). `53 02 00 01 56`. A `00` variant (`53 02 00 00 55`) also seen (start/stop). |
| `0x10` | `00` + ASCII path | **read file** (§4). `53 10 00 10 00 "/protocol.inf" <ck>` |
| `0x12` | `01` `<v>` | **SET param 0x12** = v (0/1). Used in the acquire loop; see §5. `53 04 00 12 01 00 <ck>` |
| `0x02` | `01` `00` | **SET param 0x02** = 0 → latch/trigger an acquisition (§5). `53 04 00 02 01 00 5a` |

SET form is `selector | vlen | value…` (the selector doubles as the param id).
Read-file form is `0x10 | 0x00 | <path>`.

### Payload — IN (scope → host)
`payload = selectorEcho(1) | subtype(1) | data…`, where **`selectorEcho` = request
selector OR'd with `0x80`** (`0x02`→`0x82`, `0x12`→`0x92`, `0x10`→`0x90`,
`0x01`→`0x81`). `subtype` distinguishes content (`0x01`), end-marker (`0x02`),
and size/ack (`0x00`) frames.

---

## 4. Handshake: read file (selector 0x10) — the scope self-describes

The scope's embedded Linux serves files over USB. The app reads two at startup.
**A file read returns TWO frames** and both must be consumed:

```
OUT  53 10 00 10 00 "/protocol.inf" <ck>
IN   53 <len> 90 01 <file bytes...> <ck>     ; content   (selectorEcho 0x90, subtype 0x01)
IN   53 04 00 90 02 <b> <ck>                 ; end-marker (subtype 0x02) — MUST read this
```

Two files are known; there are almost certainly more (calibration, system — see
§8). Full contents are in the appendices.

- **`/protocol.inf`** (≈3.6 KB) — an ordered list of **every setting parameter**
  the scope exposes, each with its **byte width** in the settings blob. First line
  `[TOTAL] 213` = total parameter bytes. See **Appendix A** for the complete list.
- **`/keyprotocol.inf`** (≈0.9 KB) — the list of **front-panel keys**
  (`[VT-CH1-VBSUB-KEY]`, `[HZ-TBADD-KEY]`, `[CT-AUTOSET-KEY]`, …). See
  **Appendix B**. These strongly suggest host-side control is done by **sending
  key-press events** (hypothesis — not yet captured; see §8).

---

## 5. Handshake: waveform acquisition

Per refresh the app runs (verified on hardware; samples confirmed as a 1 kHz cal
square wave):

```
OUT  53 04 00 12 01 00        ; select (value 0)     -> IN 53 04 00 92 01 00        (ack, subtype 01)
OUT  53 04 00 02 01 00        ; latch / acquire       -> IN 53 07 00 82 00 00 00 0f 00  (subtype 00 = size; 0x0F00 = 3840)
                                                      -> IN 53 04 0f 82 01 00 <3840 samples>  (subtype 01 = data)
                                                      -> IN 53 04 00 82 02 00        (subtype 02 = end-marker)
```

- **Samples: 8-bit unsigned, 1 byte each**, 3840 per block (block size depends on
  store-depth). The waveform frame payload is `82 01 00 <3840 bytes>`; the 3840
  data bytes follow the 3-byte `82 01 00` header.
- The size frame (`53 07 00 82 00 00 00 0f 00`) reports the byte count as a little
  value inside `00 00 00 0f 00` → `0x0F00 = 3840`.
- Raw counts only — **no scale is embedded**. Two levels of a cal square wave read
  as ≈`0x2E` low / ≈`0xED` high (0–255 full range). Converting counts→volts needs
  the calibration table (§8).
- **OPEN — 2-channel readout:** the `0x12` param was assumed to select the readout
  channel, but on hardware `ch=0` and `ch=1` return **byte-for-byte identical
  data**. So `0x12` does *not* switch the channel (or the block is single-channel /
  interleaved and needs de-interleaving). True 2-channel capture is unsolved.

---

## 6. Handshake: settings-state blob (poll selector 0x01)

Polling `0x01` returns a single 218-byte frame `53 d7 00 81 01 <213 data bytes> <ck>`
(`selectorEcho 0x81`, `subtype 0x01`). The app polls this continuously to mirror
the scope's live state — **this is how the app shows the correct V/div and
time/div: it reads them from this blob, it does NOT compute them from the sample
data.** Changing a front-panel knob updates the blob within one poll.

The 213 data bytes correspond to the `/protocol.inf` parameter list (its
`[TOTAL] 213`). **However, the exact byte alignment is NOT yet fully resolved:**
- The first data byte is `0x09`, but the first parameter `[VERT-CH1-DISP]` should
  be a 0/1 display flag — so the parameter region does **not** begin at the very
  first data byte; there is an undetermined prefix/header inside the blob.
- Summing `/protocol.inf` widths naively (assuming params start at data byte 0)
  does not line up with the offsets that were empirically observed to change.

**Empirically observed** (by diffing 10 blob snapshots while V/div and timebase
were changed on the front panel; offsets are into the **raw frame**, which begins
`53 d7 00 81 01`):

| raw offset(s) | observed values | notes |
|---|---|---|
| 5 | 9, 10, 0, 8 | changes with acquisition/trigger activity |
| 24 | 2, 3 | 2-state field |
| 29–30 (LE16 signed) | 33, 3, −7, 83 | a signed position (trigger-related) |
| 31–33 (LE) | 1,000,000 ↔ 1,203,000 ↔ 0 | a value field (time/holdtime/timebase-related) |
| 159, 160 | stepped 0x10→0x11→0x12 together | index field(s) that track a scaling knob |
| 217 (last) | — | frame checksum |

The working driver reads V/div-ish indices at raw 159/160 and a timebase-ish value
at raw 31–33, and those values were **consistent with the capture baseline**
(indices `0x0F`, value `1,000,000`). But mapping each raw offset to a *named*
`/protocol.inf` field is the main unfinished piece — see §8 for the method to
finish it. Treat the named interpretations above as provisional.

`decode_settings()` in `mso5202d.py` currently exposes: `status@5`, `field24@24`,
`trigpos@29`, `timebase@31`, `vdiv_ch1@159`, `vdiv_ch2@160` — using these raw
offsets. If/when the alignment is resolved, rename them to the correct fields.

---

## 7. What is verified vs. inferred

**Verified on hardware / captures (high confidence):**
- USB identity, endpoints, vendor-bulk transport.
- Frame format, length field, checksum.
- Connection recipe (detach → reset → claim → clear_halt → IN-before-write →
  persistent buffer). Reproducibly required; without reset or without
  IN-before-write it fails.
- File-read handshake (content + end-marker) and the full contents of
  `/protocol.inf` and `/keyprotocol.inf`.
- Poll → 218-byte settings blob.
- Waveform handshake and 8-bit sample format (1 kHz cal square wave confirmed).

**Inferred / open (see §8):**
- Exact settings-blob field ↔ offset mapping (there's an unmodeled prefix).
- 2-channel readout mechanism (`0x12` does not switch channels).
- Host-side control (likely key-press events via `/keyprotocol.inf`, unconfirmed).
- counts→volts and index→real-units calibration (needs a scope cal file).

---

## 8. Next reverse-engineering steps

1. **Resolve the settings-blob alignment.** Programmatically search for a prefix
   length `P` and (if needed) a field reordering such that known values land in
   the right `/protocol.inf` fields — e.g. force a *single* known change on the
   scope (only CH1 V/div) and confirm only `[VERT-CH1-VB]`'s computed offset moves.
   Repeat for `[HORIZ-TB]`, `[TRIG-VPOS]`. This nails every field.
2. **Dump more scope files** via selector `0x10`: try `/system.inf`, `/cal.inf`,
   `/calibration.inf`, `/factory.inf`, a directory listing, etc. One of these
   should hold the **counts→volts / index→"1 V/div" / timebase→seconds** tables.
   (We can now do this directly with `mso5202d.py`.)
3. **Crack 2-channel readout:** try other values/params for the pre-acquire
   select; check whether the 3840-byte block is actually interleaved (de-interleave
   even/odd) or whether a separate command fetches CH2.
4. **Host-side control:** find the command that presses a `/keyprotocol.inf` key
   (likely another selector with a key id), enabling PC control of V/div, timebase,
   trigger, autoset, single-seq, etc. A fresh capture of the app *changing settings
   from the PC* (if it supports it) would reveal this directly.
5. **Sample rate / timebase units** so the X axis becomes real seconds.

---

## 9. Implementation notes for reuse

- Driver library: `../scripts/mso5202d.py` (`Scope` class + `build`/`verify`
  framing + `decode_settings`). `python3 scripts/mso5202d.py` runs a self-test.
- Live viewer: `../scripts/mso5202d_plot.py` (matplotlib/Tk; `--png` for headless).
- Original PoC/diagnostic: `../scripts/mso5202d_probe.py`.
- Vendor Windows software (reference): `../docs/drivers/MSO5000D_Software.zip`.
- Any host implementation needs: the frame builder + checksum, the file-read /
  poll / acquire handshakes, a settings-blob parser, and an 8-bit waveform
  assembler — over libusb bulk on IN `0x81` / OUT `0x02`, with the Linux
  `cdc_subset` detach + `reset()` connection recipe (§2).

---

## Appendix A — full `/protocol.inf` (parameter list; number = byte width)

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
(Note: `[LA-*]` lines use a TAB before the width. The `SWAP`/`OVERTIME` entries
confirm this schema is shared across the MSO5000 family.)

## Appendix B — full `/keyprotocol.inf` (front-panel keys)

```
[TOTAL]  49
[START]
[FN-0-KEY] 1        [FN-1-KEY] 1        [FN-2-KEY] 1        [FN-3-KEY] 1
[FN-4-KEY] 1        [FN-5-KEY] 1        [FN-6-KEY] 1        [FN-7-KEY] 1
[FN-MLEFT-KEY] 1    [FN-MRIGHT-KEY] 1   [FN-MZERO-KEY] 1
[MENU-SR-KEY] 1     [MENU-MEASURE-KEY] 1  [MENU-ACQUIRE-KEY] 1
[MENU-UTILITY-KEY] 1  [MENU-CURSOR-KEY] 1  [MENU-DISPLAY-KEY] 1
[CT-AUTOSET-KEY] 1  [CT-SINGLESEQ-KEY] 1  [CT-RS-KEY] 1
[CT-HELP-KEY] 1     [CT-DS-KEY] 1       [CT-STU-KEY] 1
[VT-MATH-MENU-KEY] 1
[VT-CH1-MENU-KEY] 1 [VT-CH1-PSUB-KEY] 1 [VT-CH1-PADD-KEY] 1 [VT-CH1-PZERO-KEY] 1
[VT-CH1-VBSUB-KEY] 1  [VT-CH1-VBADD-KEY] 1     ; CH1 volts/div down / up
[VT-CH2-MENU-KEY] 1 [VT-CH2-PSUB-KEY] 1 [VT-CH2-PADD-KEY] 1 [VT-CH2-PZERO-KEY] 1
[VT-CH2-VBSUB-KEY] 1  [VT-CH2-VBADD-KEY] 1     ; CH2 volts/div down / up
[HZ-MENU-KEY] 1     [HZ-PSUB-KEY] 1     [HZ-PADD-KEY] 1     [HZ-PZERO-KEY] 1
[HZ-TBSUB-KEY] 1    [HZ-TBADD-KEY] 1                        ; timebase down / up
[TG-MENU-KEY] 1     [TG-PSUB-KEY] 1     [TG-PADD-KEY] 1     [TG-PZERO-KEY] 1
[TG-PHALF-KEY] 1    [TG-FORCE-KEY] 1    [TG-PROBECHECK-KEY] 1
[END]
```

## Appendix C — example raw frames (hex)

```
; --- OUT: read /protocol.inf ---
53 10 00 10 00 2f 70 72 6f 74 6f 63 6f 6c 2e 69 6e 66 66
;  S  <len=16> 10 00 "/protocol.inf"                    cksum

; --- OUT: poll (settings) ---            53 02 00 01 56
; --- OUT: acquire latch ---              53 04 00 02 01 00 5a
; --- OUT: pre-acquire select (v=0) ---   53 04 00 12 01 00 6a

; --- IN: settings-state blob (218 B), baseline ---
53 d7 00 81 01 09 00 00 00 00 00 00 ef ff 00 05 00 00 00 00 00 00 00 03
00 00 00 00 21 00 40 42 0f 00 ...  (213 data bytes total) ... <cksum>
;  S  <len=215> 81 01 <213 param bytes>                               cksum

; --- IN: waveform size then data ---
53 07 00 82 00 00 00 0f 00 eb                 ; size = 0x0F00 = 3840
53 04 0f 82 01 00 30 2d 2d 2e 30 2f 2d 2d ... ; 3840 8-bit samples
53 04 00 82 02 00 db                          ; end-marker
```
