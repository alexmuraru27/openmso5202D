# Hantek MSO5202D — USB protocol reference

**Status: transport, framing and the core handshakes are reverse-engineered and
verified against real hardware (Linux/pyusb).** A working driver (`mso5202d.py`)
connects, reads the scope's self-description, decodes live settings, and captures
waveforms (a 1 kHz cal square wave was confirmed). This document is intended to be
**self-contained** — if all other context is lost, everything needed to re-derive
and re-implement the driver is here.

Everything below was decoded from two Wireshark/USBPcap captures of the vendor
Windows app ("Scope 2.0.0.6") driving the scope, plus live experiments on the
hardware. Captures live in `../captures/` (stripped to MSO-only traffic):
`mso5202d-session1/2.pcapng` (vendor app, Windows),
`mso5202d-ch1-vdiv.pcapng` (Linux usbmon, 2026-07-08: our driver polling
settings during a full CH1 V/div knob sweep 2 mV→10 V→2 mV — the capture that
resolved the settings-blob alignment, §6), `mso5202d-timediv.pcapng`
(same setup, full time/div knob sweep over both end stops — mapped the
timebase indices, §6), `mso5202d-ch2-vdiv.pcapng` (clean CH2 V/div-only
sweep — confirmed CH2 symmetry and that TRIG-VPOS is trigger-source-bound, §6),
`mso5202d-combined.pcapng` (both channels, all four knob groups — resolved
position/trigger-level units and the ps time fields, §6), and
`mso5202d-2ch-readout.pcapng` (our driver alternating CH1/CH2 acquires — the
dual-channel readout demonstration, §5).

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
`payload = selectorEcho(1) | data…`, where **`selectorEcho` = request selector
OR'd with `0x80`** (`0x02`→`0x82`, `0x12`→`0x92`, `0x10`→`0x90`, `0x01`→`0x81`).

For **file reads (`0x90`) and waveform acquisition (`0x82`/`0x92`)** the first
data byte is a `subtype` distinguishing size/ack (`0x00`), content (`0x01`) and
end-marker (`0x02`) frames. The **settings poll response (`0x81`) has NO subtype
byte** — the 213 parameter bytes start immediately after the echo (its first
byte was long misread as "subtype 0x01"; it is actually `[VERT-CH1-DISP] = 1`.
Resolved 2026-07-08, see §6).

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
OUT  53 04 00 12 01 00        ; param 0x12 = 0 (NOT channel select; see below)
                                                      -> IN 53 04 00 92 01 00        (ack, subtype 01)
OUT  53 04 00 02 01 <ch>      ; acquire CHANNEL <ch>: 00 = CH1, 01 = CH2
                                                      -> IN 53 07 00 82 00 00 00 0f 00  (subtype 00 = size; 0x0F00 = 3840)
                                                      -> IN 53 04 0f 82 01 00 <3840 samples>  (subtype 01 = data)
                                                      -> IN 53 04 00 82 02 00        (subtype 02 = end-marker)
```

- **Samples: 8-bit unsigned, 1 byte each**, 3840 per block (block size depends on
  store-depth). The waveform frame payload is `82 01 00 <3840 bytes>`; the 3840
  data bytes follow the 3-byte `82 01 00` header.
- **Sample polarity is INVERTED relative to the screen**: a larger count = a
  lower trace (confirmed on hardware: CH2 parked at −0.64 div read ≈192, i.e.
  above mid-scale 128; the on-screen up/down order of two traces is the mirror
  of their count order). Display code should flip (e.g. plot `255 − sample` or
  invert the Y axis).
- The size frame (`53 07 00 82 00 00 00 0f 00`) reports the byte count as a little
  value inside `00 00 00 0f 00` → `0x0F00 = 3840`.
- Raw counts only — **no scale is embedded**. Two levels of a cal square wave read
  as ≈`0x2E` low / ≈`0xED` high (0–255 full range). Converting counts→volts needs
  the calibration table (§8).
- **2-channel readout — SOLVED (2026-07-08):** the channel is selected by the
  **acquire value byte**: `02 01 00` = CH1, `02 01 01` = CH2. Verified on
  hardware with CH2's probe disconnected (CH1 returned the square wave, CH2 its
  flat line); `../captures/mso5202d-2ch-readout.pcapng` records 6 alternating
  CH1/CH2 acquire pairs with their distinct 3840-sample responses. The `0x12` param is **not** a channel select — early tests
  varying it returned identical data because it does something else entirely
  (the vendor app toggles it `1` → `0` around every refresh; run/hold?).
  Values `12 01 02`/`03` make the next acquire return nothing. Note the vendor
  captures (`session1/2`) were taken with **CH2 display off**, which is why
  they never showed a CH2 fetch.
- **OPEN — counts↔volts scaling:** the transfer amplitude does not track the
  display V/div (a 5 V cal square read ≈70 counts p-p at 2 V/div but ≈233 p-p
  in an earlier read at a different setting). A **flat** (no-signal) trace
  follows `count = 128 − POS` exactly (verified at POS −64 → 192 and
  POS +62 → 63; i.e. 1 count = 1/100 div, inverted), while a **live** 5 V
  square at 2 V/div spans only ≈28 counts/div — flat and live traces obey
  different scale factors. The transfer gain/offset model needs dedicated
  experiments (§8).

---

## 6. Handshake: settings-state blob (poll selector 0x01) — alignment RESOLVED

Polling `0x01` returns a single 218-byte frame `53 d7 00 81 <213 param bytes> <ck>`
(`selectorEcho 0x81`, **no subtype byte**). The app polls this continuously to
mirror the scope's live state — **this is how the app shows the correct V/div and
time/div: it reads them from this blob, it does NOT compute them from the sample
data.** Changing a front-panel knob updates the blob within one poll.

**The alignment is fully resolved (2026-07-08):** the 213 data bytes are exactly
the `/protocol.inf` parameter list (`[TOTAL] 213`, Appendix A), starting
**immediately after the `0x81` echo** — i.e. at **raw frame offset 4** — with no
prefix and no reordering. The old "unmodeled prefix" mystery was a misparse: the
first data byte (`0x01`) was taken to be a frame subtype, when it is actually the
first parameter `[VERT-CH1-DISP] = 1` (CH1 shown). Offset of any field = `4 +`
the sum of the widths of all parameters before it, multi-byte fields
little-endian (positions/levels signed).

Proof — a 60 s capture of a full CH1 V/div knob sweep, 2 mV → 10 V and back
(`captures/mso5202d-ch1-vdiv.pcapng`, 323 settings frames): across the whole
sweep **only four raw offsets changed**, and each lands exactly on the computed
offset of a field that *should* react:

| raw offset | computed field | observed |
|---|---|---|
| 5 | `[VERT-CH1-VB]` | V/div index, one step per click — see table below |
| 24 | `[TRIG-STATE]` | 3→2→3 blips as the trigger dropped in/out at extreme V/div |
| 29–30 | `[TRIG-VPOS]` (LE16) | trigger level in **screen-relative units**: rescaled as 1/V-div on every click because the absolute trigger voltage stayed fixed (`VPOS = 62000 // vdiv_mV` throughout that session) |
| — | (nothing else moved) | `[VERT-CH2-VB]`@15 stayed 9, `[HORIZ-TB]`@159 stayed 15 ✓ |

Additional confirmations from the same frames: `[VERT-CH2-DISP]`@14 = 0 (CH2 was
off), `[TRIG-SRC]`@26 = 0 (CH1), `[TRIG-FREQUENCY]`@31–38 (LE64) = 1,000,000 =
the 1 kHz cal signal **in mHz** (this had been mislabeled "timebase"), and
`[HORIZ-TB]`@159 / `[HORIZ-WIN-TB]`@160 are the real timebase indices (mislabeled
"vdiv" before; their index→time/div table is still unmapped, see §8).

**`[VERT-CHx-VB]` → V/div** (verified over the full range on hardware):

| VB | V/div | VB | V/div | VB | V/div |
|---|---|---|---|---|---|
| 0 | 2 mV | 4 | 50 mV | 8 | 1 V |
| 1 | 5 mV | 5 | 100 mV | 9 | 2 V |
| 2 | 10 mV | 6 | 200 mV | 10 | 5 V |
| 3 | 20 mV | 7 | 500 mV | 0 | **10 V (quirk: wraps mod 11!)** |

The 10 V/div position re-uses VB=0 — a firmware quirk. If it matters, a nonzero
`[TRIG-VPOS]` disambiguates (it scales as 1/V-div for a fixed trigger level).
The table and the wrap quirk were confirmed **identical for CH2** by a clean CH2
V/div sweep (`captures/mso5202d-ch2-vdiv.pcapng`, 2026-07-08): across 324
frames the **only** changing offset was `[VERT-CH2-VB]`@15 — notably
`[TRIG-VPOS]` did **not** move, so VPOS is bound to the **trigger source** (CH1
here), not to the channel being adjusted.

**Vertical position & trigger level — RESOLVED** (combined-knobs capture
`captures/mso5202d-combined.pcapng`, 2026-07-08: both channels on; V/div,
vertical position, time/div and horizontal position all exercised):

- `[VERT-CHx-POS]` (signed LE16) is the channel's vertical position in
  **1/100-division units** (knob fine step = 8 = 0.08 div).
- `[TRIG-VPOS]` (signed LE16) = **`[VERT-CHsrc-POS]` + trigger level ⁄ V-div ×
  100** — i.e. the trigger marker's screen position in the same 1/100-div
  units. Proof: over a CH1 V/div sweep 5 V→100 mV with CH1-POS = −4,
  `(VPOS − POS) × vdiv/100` = 780 mV **constant at every step**; VPOS tracked
  the CH1 position knob exactly 1:1; CH2's knobs never moved it. (This also
  retro-explains the `62000 // vdiv_mV` fit in the first sweep: level 620 mV,
  POS 0.) Derived `TRIG-LEVEL-mV` is computed in `decode_settings()`.

**8-byte time fields are PICOSECONDS.** `[TRIG-HOLDTIME-MIN]` = 100 000 =
100 ns and `[TRIG-HOLDTIME-MAX]` = 10¹³ = 10 s — exactly the scope's holdoff
limits. `[HORIZ-TRIGTIME]`@162–169 is the **horizontal trigger position
(delay) in ps**: in the combined capture each click moved it by ≈0.3 div
worth of time at every timebase tried (e.g. ±240 µs at 800 µs/div, ±1.2 ms at
4 ms/div). The `TRIG-*-TIME` family (pulse/slope/overtime) reads sanely in ps
too (e.g. 500 000 = 500 ns defaults).

**`[HORIZ-TB]` / `[HORIZ-WIN-TB]` → time/div** (verified end stop to end stop by
a full time/div knob sweep, `captures/mso5202d-timediv.pcapng`, 2026-07-08; only
raw@159, raw@160 and `[TRIG-STATE]`@24 changed across 810 frames):

- The knob has **32 positions** = the scope's 2 ns…40 s range in the **2-4-8
  sequence** (2, 4, 8, 20, 40, 80, 200 ns, …, 8, 20, 40 s). Confirmed against
  the on-screen readout — it is NOT the usual 1-2-4/1-2-5 sequence (8 ns, not
  10 ns; 80 µs, not 100 µs; …).
- **`[HORIZ-WIN-TB]`@160 tracks the knob over the full index range 0..31**
  (0 = 2 ns/div … 31 = 40 s/div).
- **`[HORIZ-TB]`@159 is the real acquisition timebase and clamps at index 6**
  (200 ns/div): for the six fastest settings (80 ns…2 ns) only WIN-TB keeps
  falling — the fast timebases are zoom/interpolation over a 200 ns/div
  acquisition (a known trait of this scope family). At index ≥ 6 the two move
  in lockstep (transient ±1 skews right around the clamp boundary).
- `index → ns`: `TB_TO_NS` table in `mso5202d.py` ((2, 4, 8)·10ⁿ).

**`[TRIG-STATE]` observed values** (all sweep captures): `3` = triggered/run,
`6` = transient flicker while re-arming (appears in bursts when the signal
drops out of range mid-adjustment), `4` = **scan/roll mode**, persistent at
slow timebases (onset varies with trigger activity — seen from 80 ms/div in
one session, from 2 s/div in another). Enum not exhaustively mapped.

`decode_settings()` in `mso5202d.py` now decodes the **entire blob** into named
`/protocol.inf` fields (plus derived `CH1-VDIV-mV`/`CH2-VDIV-mV`, `TDIV-ns`
(knob, from WIN-TB) and `TDIV-ACQ-ns` (real acquisition TB)), driven by a
`SETTINGS_PARAMS` (name, width) table transcribed from Appendix A.

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
- Poll → 218-byte settings blob, and its **full field ↔ offset mapping**: the
  213 param bytes are the `/protocol.inf` list verbatim, starting at raw
  offset 4 (§6; proven by the CH1-V/div sweep capture, 2026-07-08). Includes
  the `[VERT-CHx-VB]` → V/div table (2 mV…10 V, with the 10 V → VB=0 wrap
  quirk; verified on **both channels**), `[VERT-CHx-POS]` and `[TRIG-VPOS]` in
  signed 1/100-div units (`VPOS = POS_src + level/vdiv×100`), 8-byte time
  fields in **picoseconds** (incl. `[HORIZ-TRIGTIME]` = horizontal delay), and
  `[TRIG-FREQUENCY]` in mHz.
- Waveform handshake and 8-bit sample format (1 kHz cal square wave confirmed).

- `[HORIZ-TB]`/`[HORIZ-WIN-TB]` index → time/div table (2 ns…40 s, **2-4-8**
  sequence, 32 steps; WIN-TB = knob, HORIZ-TB clamps at 200 ns — §6, proven by
  the time/div sweep capture + on-screen readings).

- **2-channel readout**: acquire value byte selects the channel (`02 01 <ch>`,
  0 = CH1, 1 = CH2) — verified square-vs-flat on hardware (§5).

**Inferred / open (see §8):**
- Enum-coded field values (coupling, trigger modes, `[TRIG-STATE]` beyond
  {3 run, 4 scan, 6 re-arm}, …).
- Meaning of param `0x12` (vendor toggles it 1→0 per refresh; run/hold?).
- Host-side control (likely key-press events via `/keyprotocol.inf`, unconfirmed).
- counts→volts transfer scaling (does not track display V/div — §5) and
  calibration (may need a scope cal file).

---

## 8. Next reverse-engineering steps

1. ~~**Resolve the settings-blob alignment.**~~ **DONE (2026-07-08)** — exactly by
   the method described here: a single-knob CH1 V/div sweep capture
   (`captures/mso5202d-ch1-vdiv.pcapng`) moved only `[VERT-CH1-VB]`,
   `[TRIG-STATE]` and `[TRIG-VPOS]` at their computed offsets → params start at
   raw offset 4, no prefix (§6). The *timebase* sweep followed the same day
   (`captures/mso5202d-timediv.pcapng`) and mapped `[HORIZ-TB]`/`[HORIZ-WIN-TB]`
   → time/div (§6); a CH2 sweep confirmed channel symmetry, and a combined
   both-channels/all-knobs capture resolved position + trigger-level units
   (1/100 div) and the ps time fields (§6). **Remaining follow-up of the same
   shape:** toggle coupling / trigger-mode / acquire menus to enumerate the
   enum-coded fields.
2. **Dump more scope files** via selector `0x10`: try `/system.inf`, `/cal.inf`,
   `/calibration.inf`, `/factory.inf`, a directory listing, etc. One of these
   should hold the **counts→volts / index→"1 V/div" / timebase→seconds** tables.
   (We can now do this directly with `mso5202d.py`.)
3. ~~**Crack 2-channel readout**~~ **DONE (2026-07-08)** — the acquire value
   byte is the channel: `02 01 00` = CH1, `02 01 01` = CH2 (§5). Follow-up:
   figure out the **counts↔volts transfer scaling** (it does not track the
   display V/div; vary V-div/position with a known signal and model
   gain/offset), and what param `0x12` actually does.
4. **Host-side control:** find the command that presses a `/keyprotocol.inf` key
   (likely another selector with a key id), enabling PC control of V/div, timebase,
   trigger, autoset, single-seq, etc. A fresh capture of the app *changing settings
   from the PC* (if it supports it) would reveal this directly.
5. **Sample rate** so the X axis becomes real seconds: time/div is now known
   (§6) — what's left is how many divisions (or seconds) the 3840-sample block
   spans. **Preliminary:** counting 1 kHz cal cycles per block gives ~4 cycles
   at 200 µs/div and ~8 at 400 µs/div → the block spans ≈ 20 divisions,
   i.e. **192 samples/div** (needs confirming at more timebases / vs the
   store-depth setting).

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
