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
dual-channel readout demonstration, §5), `mso5202d-ch1-vpos.pcapng` (fixed
5 V/1 kHz trace moved on- and off-screen — established the screen-rendered
readout scale and off-screen rail-clipping, §5), `mso5202d-trig-level.pcapng`
(full trigger-level sweep — scope-calibrated the 1/25-div level unit, §6), `mso5202d-trig-buttons.pcapng` (Set-50 % / Force-Trig / Run-Stop — `TRIG-STATE`
0/1 values and the Set-50 % behaviour, §6), `mso5202d-trig-runstop.pcapng`
(a known stop/run/single/force sequence — mapped `TRIG-STATE` 0=STOP, 5=SINGLE,
§6), `mso5202d-trig-type.pcapng` (stepping the trigger-type menu — mapped
`TRIG-TYPE` Edge/Video/Pulse/Slope/Overtime/Alter, §6), and
`mso5202d-trig-edge.pcapng` (Edge-trigger source/slope/mode/coupling sweep —
mapped `TRIG-SRC`, `TRIG-EDGE-SLOPE`, `TRIG-MODE`, `TRIG-COUP`, and resolved
`TRIG-STATE`=1=WAIT, §6), `mso5202d-trig-video.pcapng` (Video-trigger
source/polarity/standard/sync sweep — mapped the `TRIG-VIDEO-*` fields, §6), and
`mso5202d-trig-slope.pcapng` (Slope-trigger set/window/V1/V2/when/time sweep —
mapped the `TRIG-SLOPE-*` fields, §6), `mso5202d-trig-pulse.pcapng`
(Pulse-trigger polarity/when/width sweep — mapped the `TRIG-PULSE-*` fields, §6),
`mso5202d-trig-overtime.pcapng` (Overtime-trigger polarity/time/coupling
sweep — mapped the `TRIG-OVERTIME-*` fields and menu id 39, §6), and
`mso5202d-trig-alter-ch1.pcapng` (Alter/Swap CH1 per-type config — mapped
`TRIG-SWAP-CHx-TYPE` and the per-channel sub-params + menu ids 26–29, §6), and
`mso5202d-trig-alter-ch2.pcapng` (Alter/Swap CH2 — confirmed CH2 symmetry, menu
ids 30–33, §6), `mso5202d-trig-holdoff.pcapng` (holdoff-knob sweep —
confirmed `TRIG-HOLDTIME` tracks the knob, §6), `mso5202d-trig-knob.pcapng`
(trigger-level knob push — isolated it as "level to 0 V / channel ground",
distinct from Set-50 %, §6), and `mso5202d-horiz-menu.pcapng` (Horizontal menu —
mapped menu ids 3/40 and the LA menu 61; window/mark controls don't touch the
settings blob, §6), `mso5202d-horiz-position.pcapng` (horizontal-position
knob sweep — showed `HORIZ-TRIGTIME` is SIGNED, −4 ms…+29 ms, §6),
`mso5202d-acquire.pcapng` (Acquire menu — mapped `ACQURIE-TYPE/MODE/AVG-CNT/
STORE-DEPTH`, §6), `mso5202d-acquire-1m.pcapng` (single-channel — captured
the 1M store-depth code 7, §6), `mso5202d-ch1-menu.pcapng` (CH1 vertical
menu — mapped `VERT-CHx-COUP/20MHZ/FINE/PROBE/RPHASE`, §6), `mso5202d-ch2-menu.pcapng` (CH2 vertical menu — confirmed channel symmetry, §6),
`mso5202d-pos-knob-push.pcapng` (position-knob pushes — vertical and
horizontal position knobs reset their axis to 0/centre, §6), `mso5202d-autoset.pcapng` (AUTOSET button — compound reconfigure; §6 note), and
`mso5202d-display.pcapng` (Display menu — mapped `DISPLAY-MODE/FORMAT/GRID-KIND/
GRID-BRIGHT/CONTRAST/PERSIST` + menu ids 4/36, §6), `mso5202d-cursor.pcapng`
(Cursor menu — cursor state NOT in the blob; got menu id 15, plus `MATH-DISP`
and Math menu id 41, §6), and `mso5202d-math.pcapng` (Math menu — mapped
`MATH-MODE` + FFT `SRC/WIN/FACTOR/DB` and menu ids 16/56; DISPLAY-FORMAT=2=FFT, §6).

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
- **Sample polarity (screen layout): smaller count = HIGHER on screen.**
  Matching the scope's static layout needs an *inverted* Y axis: with CH2 at the
  top of the scope reading bytes ~17–85 and CH1 at the bottom reading ~161–189,
  the smaller bytes belong up top. **Caveat / open:** while a trace is *moved*
  the byte can track the *opposite* way (a controlled on-screen raise of CH1
  drove `VERT-CH1-POS` −82→−20 while the byte went 184→239, i.e. the wrong
  direction for the static rule) — the vertical byte↔position mapping is
  self-inconsistent, entangled with the unresolved amplitude-scaling bug below.
  Match the static layout (invert); don't trust it for absolute volts yet.
- **Horizontal scale: 200 samples per division** → sample interval =
  `TDIV / 200`, sample rate = `200 / TDIV`. So a 3840-sample block spans
  **19.2 divisions**. The vendor MSO5000-series manual states "sample interval
  = s/div ÷ 200"; confirmed to the digit on our hardware (500 samples/period
  of the 1 kHz cal at 400 µs/div → 2 µs interval → 0.5 MSa/s, and the cal
  cycle count per block matches at every timebase). `decode_settings()` exposes
  `SAMPLE-INTERVAL-ns` and `SAMPLERATE-HZ`; the block max rate is 500 MSa/s
  (dual-channel) per the manual.
- The size frame (`53 07 00 82 00 00 00 0f 00`) reports the byte count as a little
  value inside `00 00 00 0f 00` → `0x0F00 = 3840`.
- 8-bit samples, screen-oriented (see the polarity note below). Beyond that the
  **vertical amplitude scale is NOT yet modelled** — see the open item below;
  don't assume a fixed counts/div.
- **Off-screen positioning yields CLIPPED / invalid bytes.** Part of a trace
  moved past a graticule edge clamps to that rail (top ≈ `0x0A`, bottom
  ≈ `0xF2`), and while a trace is scrolled across/through the screen a block can
  come back **rail-to-rail bimodal** — ~50 % of samples at each rail, nothing in
  between (the "full-height hash" seen when moving a trace off and back on
  screen). In `../captures/mso5202d-ch1-vpos.pcapng` such blocks show e.g. 1919
  samples at `0x0A` + 1921 at `0xF2`. **Display/decoding code must treat
  rail-pinned samples (≈0/≈255) as clipped, not real** and can flag "off
  screen" (the plotter does).
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
- **OPEN — vertical amplitude depends on the trace's DISTANCE FROM SCREEN
  CENTRE (the "0 axis").** The returned amplitude is **full-swing (~200+ counts
  p-p) when the trace baseline sits near the centre/0 line, and compressed
  (~25 counts) when it is parked well away from centre.** User-observed live
  ("when it crosses the 0 axis the app goes full-screen") and confirmed by
  measurement (2026-07-08): with both channels on the same 5 V/1 kHz cal,
  CH1 @ 5 V/div at `POS=-78` (far below centre) → **25 counts**, while
  CH2 @ 2 V/div at `POS=-16` (near centre) → **217 counts**. Read repeatedly the
  values are stable per position. So the byte encoding appears to be referenced
  to screen centre at a high fixed gain, not to the channel's positioned
  baseline — moving the trace off-centre shrinks/rescales the returned swing
  (and also drives the "raise → app-trace-descends" polarity anomaly). This is a
  *characterisation, not a model.* Ruled out: `VERT-CHx-FINE=0`,
  `VERT-CHx-PROBE=0`, `TRIG-STATE=3`, acquire mode/type (all identical across
  channels). Vendor-manual context: 8-bit ADC per channel, DC gain ±3%,
  graticule 8 div tall. Resolve with a controlled sweep that logs amplitude &
  baseline vs `VERT-POS` (one channel, fixed signal) to pin the amplitude =
  f(distance-from-centre) law, then a V/div sweep at fixed centre position (§8).
- **OPEN — inter-channel PHASE is not preserved.** CH1 and CH2 are fetched as
  two separate acquires (~100 ms apart) with no shared trigger lock, so their
  returned phase is uncorrelated: the CH1→CH2 first-rising-edge offset jittered
  66/89/30/94 samples across reads (one 1 kHz period = 500 samples at
  400 µs/div). On the scope the two channels are sampled simultaneously and look
  in-phase; our plotter shows them phase-shifted. A single-acquire "both
  channels" command (if one exists) would fix it; otherwise the readout can't
  reproduce cross-channel timing.

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
  **1/25-division units** (knob fine step = 8 = 0.32 div; see the calibration
  below). **Pushing the vertical-position knob resets it to 0** (channel to
  vertical centre) — `mso5202d-pos-knob-push.pcapng`, matching the manual.
- `[TRIG-VPOS]` (signed LE16) = **`[VERT-CHsrc-POS]` + trigger level ⁄ V-div ×
  25** — i.e. the trigger marker's screen position, where the position/level
  fields are in **1/25-division units** (see below). VPOS tracks the CH1
  position knob 1:1 and CH2's knobs never move it, confirming it is bound to the
  trigger *source*. Derived `TRIG-LEVEL-mV` is computed in `decode_settings()`.

  **Unit = 1/25 division, calibrated against the scope readout (2026-07-08).**
  A full trigger-level sweep clamped `TRIG-VPOS` at **±200**, which the scope
  displayed as **+13.4 V … −18.5 V at 2 V/div** and **+33.6 V … −46.4 V at
  5 V/div** (`../captures/mso5202d-trig-level.pcapng`, CH1 POS +32). Both fit
  `level = (VPOS − POS) × vdiv / 25` to <1 %:
  (200−32)/25×2 V = +13.44 V, (−200−32)/25×2 V = −18.56 V; ×5 V → +33.6/−46.4 V.
  So **±200 = ±8 divisions** (200/25), matching the manual's "±8 divisions from
  centre" trigger range — and the general rule is `1 unit = 1/25 div`
  (25 counts/div), used for `VERT-CHx-POS`, `TRIG-VPOS` and the trigger level.
  *(Corrects an earlier guess of 1/100-div here — it was never scope-calibrated;
  the ×4 error surfaced when the scope's V readout was compared.)*
  Two distinct level-reset controls, verified separately:
  - The front-panel **"Set 50 %"** softkey snaps `TRIG-VPOS` to the signal's
    **mid-amplitude**: in `../captures/mso5202d-trig-buttons.pcapng` six presses
    from different starting levels all landed on VPOS 63 = **2480 mV ≈ 50 % of
    the 5 V cal**.
  - **Pushing the trigger-level knob** snaps `TRIG-VPOS` to the **source
    channel's 0 V / ground** (`TRIG-VPOS := VERT-CHsrc-POS`, level = 0 mV):
    in `../captures/mso5202d-trig-knob.pcapng` every push from various levels
    (VPOS 54/41/48) landed on VPOS 35 = CH1 POS = **0 mV**. So the knob-push is
    "level to ground", *not* Set-50 %.

**8-byte time fields are PICOSECONDS.** `[TRIG-HOLDTIME-MIN]` = 100 000 =
100 ns and `[TRIG-HOLDTIME-MAX]` = 10¹³ = 10 s — exactly the scope's holdoff
limits (the vendor manual lists **Holdoff Range 100 ns to 10 s**, confirming
both the unit and the field). `[TRIG-HOLDTIME]` is the **live holdoff value**,
verified to track the knob (`mso5202d-trig-holdoff.pcapng`): it lives under the
**HORIZONTAL menu → F4 (Holdoff Time)** — turn the multi-function knob to adjust,
push it to reset to 100 ns (the observed floor). The Horizontal menu is
`[CONTROL-MENUID]` **3 (page 1)** / **40 (page 2, holdoff)`
(`mso5202d-horiz-menu.pcapng`). **Window ctrl (Major/Minor), Mark right/left,
Set/Clear, Clear-all produce NO change in the polled settings blob** —
`[HORIZ-WIN-STATE]` never moved over 2 min of trying, so the dual-window/zoom
and mark state must live in a separate structure (not the 213-byte state), or
require dual-window mode to be actively engaged first. The horizontal *position*
knob moves `[HORIZ-TRIGTIME]` (delay, ps). `[HORIZ-TRIGTIME]`@162–169 is the **horizontal
trigger position (delay) in ps**: in the combined capture each click moved it
by ≈0.3 div worth of time at every timebase tried (e.g. ±240 µs at 800 µs/div,
±1.2 ms at 4 ms/div). It is **SIGNED** (int64) — the horizontal-position sweep
in `mso5202d-horiz-position.pcapng` drove it from ≈−4 ms through 0 to +29 ms;
negative values (post-trigger) show up as ≈2⁶⁴−x if read unsigned, so
`decode_settings()` treats it as signed. **Pushing the horizontal-position knob
resets it to 0** (delay to centre) — `mso5202d-pos-knob-push.pcapng`. The `TRIG-*-TIME` family (pulse/slope/overtime) reads
sanely in ps too (500 000 = 500 ns defaults; the manual gives pulse/slope/
overtime ranges of **20 ns to 10 s**).

**`[HORIZ-TB]` / `[HORIZ-WIN-TB]` → time/div** (verified end stop to end stop by
a full time/div knob sweep, `captures/mso5202d-timediv.pcapng`, 2026-07-08; only
raw@159, raw@160 and `[TRIG-STATE]`@24 changed across 810 frames):

- The knob has **32 positions** = the scope's 2 ns…40 s range in the **2-4-8
  sequence** (2, 4, 8, 20, 40, 80, 200 ns, …, 8, 20, 40 s). Confirmed against
  the on-screen readout — it is NOT the usual 1-2-4/1-2-5 sequence (8 ns, not
  10 ns; 80 µs, not 100 µs; …). The vendor manual states **"SEC/DIV Range:
  4 ns/div to 40 s/div, in a 2, 4, 8 sequence"** — matching the sequence; the
  manual is for the 60/100 MHz MSO5000B base model (min 4 ns/div, index 1),
  while our 200 MHz MSO5202D extends one detent faster to 2 ns/div (index 0).
- **`[HORIZ-WIN-TB]`@160 tracks the knob over the full index range 0..31**
  (0 = 2 ns/div … 31 = 40 s/div).
- **`[HORIZ-TB]`@159 is the real acquisition timebase and clamps at index 6**
  (200 ns/div): for the six fastest settings (80 ns…2 ns) only WIN-TB keeps
  falling — the fast timebases are zoom/interpolation over a 200 ns/div
  acquisition (a known trait of this scope family). At index ≥ 6 the two move
  in lockstep (transient ±1 skews right around the clamp boundary).
- `index → ns`: `TB_TO_NS` table in `mso5202d.py` ((2, 4, 8)·10ⁿ).

**`[TRIG-STATE]` observed values** — mapped by a known button sequence
(`mso5202d-trig-runstop.pcapng`: stop→run→stop→run→single→run→stop→run→force):
- `0` = **STOPPED** — pressing Run/Stop takes 3→0 (confirmed 3×). Run resumes
  via 0→2→3.
- `2` = **not triggered / auto-searching** (free-running in Auto with no valid
  trigger; also seen when the level is swept off the signal in
  `mso5202d-trig-level.pcapng`, 3→2→3).
- `3` = **triggered / running**. Force-Trigger while already running causes no
  state change (just forces an acquire).
- `4` = **scan/roll mode** (slow timebase, see below).
- `5` = **SINGLE (armed)** — Single-Seq takes 3→5; the next Run leaves it 5→2→3.
- `6` = transient flicker while re-arming (bursts when the signal drops out of
  range mid-adjustment).
- `1` = **WAIT / READY** (Normal mode, armed but no valid trigger yet) —
  resolved in `mso5202d-trig-edge.pcapng`: setting `TRIG-MODE`→Normal with the
  level off the signal took `TRIG-STATE`→1, and Auto→2. So the untriggered state
  is **1 in Normal mode, 2 in Auto mode**.

Run/Stop/Single state lives in this field, not a separate one
(`TRIG-MODE`/`ACQURIE-MODE` never changed). The manual pins
the scan-mode threshold: *"With the SEC/DIV control set to 80 ms/div or slower
and the trigger mode set to Auto, the oscilloscope works in the scan
acquisition mode."* — i.e. `[HORIZ-WIN-TB]` index ≥ 23 (80 ms/div), which
matches our earliest `state=4` onset; the later onset in another session was
Normal-mode trigger holding `state=3` longer. Enum not exhaustively mapped.

**`[TRIG-TYPE]` enum** — mapped by stepping the trigger-type menu twice
(`mso5202d-trig-type.pcapng`, both passes identical): `0` = **Edge**,
`1` = **Video**, `2` = **Pulse**, `3` = **Slope**, `4` = **Overtime**,
`5` = **Alter** (swap — at this type `[TRIG-SRC]` rapidly toggles 0↔1 as the
alternating trigger flips between CH1/CH2). `[CONTROL-MENUID]` follows each
type's submenu (Edge 5, Video 8, Pulse 6, Slope 22, O.T. 38, Alter 24), and
opening the trigger menu sets `[CONTROL-DISP-MENU]` 0→1. (`TRIG_TYPE_NAMES` in
`mso5202d.py`.)

**Edge-trigger enums** — mapped by stepping the Edge trigger menu twice
(`mso5202d-trig-edge.pcapng`, both passes identical):
- `[TRIG-SRC]`: `0` = **CH1**, `1` = **CH2**, `2` = **EXT**, `3` = **EXT/5**,
  `4` = **AC line**. (`TRIG-LEVEL-mV` is only derived for CH1/CH2; for EXT the
  manual range is ±1.2 V, EXT/5 ±6 V.)
- `[TRIG-MODE]`: `0` = **Auto**, `1` = **Normal**. Setting it also mirrors into
  `[TRIG-SWAP-CH1-MODE]`/`[TRIG-SWAP-CH2-MODE]`. (Determines the untriggered
  `TRIG-STATE`: Auto→2, Normal→1.)
- `[TRIG-EDGE-SLOPE]`: `0` = **Rising**, `1` = **Falling**.
- `[TRIG-COUP]`: `0` = **DC**, `1` = **AC**, `2` = **Noise Reject**,
  `3` = **HF Reject**, `4` = **LF Reject**.

(`TRIG_SRC_NAMES` / `TRIG_MODE_NAMES` / `TRIG_SLOPE_NAMES` / `TRIG_COUP_NAMES`
in `mso5202d.py`.)

**Video-trigger enums** — mapped by stepping the Video trigger menu
(`mso5202d-trig-video.pcapng`). Video sources are CH1/CH2/EXT/EXT/5 (no AC line):
- `[TRIG-VIDEO-NEG]`: `0` = **Normal** (positive video), `1` = **Inverted**.
- `[TRIG-VIDEO-PAL]`: `0` = **NTSC**, `1` = **PAL/SECAM**.
- `[TRIG-VIDEO-SYN]`: `0` = **All Lines**, `1` = **Line Num**, `2` = **Odd
  Field**, `3` = **Even Field**, `4` = **All Fields**.
- `[TRIG-VIDEO-LINE]` (LE16): selected line number, active when SYN = Line Num;
  range **1…525** for NTSC (1…625 for PAL/SECAM).

(`TRIG_VIDEO_NEG_NAMES` / `TRIG_VIDEO_STD_NAMES` / `TRIG_VIDEO_SYN_NAMES` in
`mso5202d.py`.)

**Slope-trigger enums** — mapped by stepping the Slope trigger menu
(`mso5202d-trig-slope.pcapng`; source/mode/coupling behave as for Edge):
- `[TRIG-SLOPE-SET]`: `0` = **Positive** slope, `1` = **Negative**.
- `[TRIG-SLOPE-WIN]`: `0` = **V1** (upper), `1` = **V2** (lower), `2` = **Both**
  — selects which threshold the knob adjusts (at Both, V1 and V2 move together).
- `[TRIG-SLOPE-V1]`, `[TRIG-SLOPE-V2]` (signed LE16): the two slope thresholds
  in **1/25-div** units, using the **same volts calibration as the trigger
  level** — scope-verified at CH1 5 V/div (V1/V2 range read **+12 V … −36 V**,
  matching `(field − POS) × vdiv/25`). `decode_settings()` exposes derived
  `TRIG-SLOPE-V1-mV` / `TRIG-SLOPE-V2-mV`.
- `[TRIG-SLOPE-WHEN]`: `0` = **=**, `1` = **≠**, `2` = **>**, `3` = **<** (the
  slope-time condition; likely the same enum as `[TRIG-PULSE-WHEN]`).
- `[TRIG-SLOPE-TIME]` (ps): 20 ns (`20000`) … 10 s.

(`TRIG_SLOPE_SET_NAMES` / `TRIG_SLOPE_WIN_NAMES` / `TRIG_WHEN_NAMES` in
`mso5202d.py`.)

**Pulse-trigger enums** — mapped by stepping the Pulse trigger menu
(`mso5202d-trig-pulse.pcapng`):
- `[TRIG-PULSE-NEG]`: `0` = **Positive** pulse, `1` = **Negative**.
- `[TRIG-PULSE-WHEN]`: `0` = **=**, `1` = **≠**, `2` = **>**, `3` = **<**
  — **confirmed identical to `[TRIG-SLOPE-WHEN]`** (shared `TRIG_WHEN_NAMES`).
- `[TRIG-PULSE-TIME]` (ps): pulse width, **20 ns (`20000`) … 10 s** (both limits
  hardware-confirmed).

(`TRIG_PULSE_NEG_NAMES` in `mso5202d.py`.)

**Overtime-trigger enums** — mapped by stepping the O.T. trigger menu
(`mso5202d-trig-overtime.pcapng`):
- `[TRIG-OVERTIME-NEG]`: `0` = **Positive**, `1` = **Negative**.
- `[TRIG-OVERTIME-TIME]` (ps): the overtime, **20 ns … 10 s**.
- Coupling (page 2) uses the same `[TRIG-COUP]` enum as Edge. New menu id:
  `[CONTROL-MENUID]` 38 = Overtime page 1, **39 = page 2** (38/39 consecutive,
  as with Pulse 6/7 and Slope 22/23).

(`TRIG_OVERTIME_NEG_NAMES` in `mso5202d.py`.)

**Alter/Swap trigger** (`mso5202d-trig-alter-ch1.pcapng`) — in Alter mode each
channel carries its **own independent trigger config** in the
`[TRIG-SWAP-CH1-*]` / `[TRIG-SWAP-CH2-*]` blocks (offsets 94–121 / 122–149), and
`[TRIG-SRC]` alternates CH1↔CH2 as the scope switches between them.
- `[TRIG-SWAP-CHx-TYPE]`: a **4-value** enum (no Slope/Alter): `0` = **Edge**,
  `1` = **Video**, `2` = **Pulse**, `3` = **Overtime**. (Distinct from the main
  6-value `[TRIG-TYPE]`.)
- All the per-channel sub-params **reuse the main-trigger enums**:
  `[TRIG-SWAP-CHx-EDGE-SLOPE]` = 0 Rising/1 Falling, `-COUP` = the 5-value
  `TRIG-COUP` set, `-MODE` = Auto/Normal, `-VIDEO-NEG/PAL/SYN/LINE` = the video
  enums, `-PULSE-NEG/WHEN/TIME` and `-OVERTIME-NEG/TIME` = the pulse/overtime
  fields (ps times). Verified independently on **both channels** (CH1 and CH2
  blocks are identical layout).
- Menu ids for the Alter per-type submenus (`[CONTROL-MENUID]`): 24 = Alter
  base; **CH1** block **26 Edge / 27 Pulse / 28 Video / 29 Overtime**; **CH2**
  block **30 Edge / 31 Pulse / 32 Video / 33 Overtime** (so the menu id encodes
  both the selected channel and its type).

(`TRIG_SWAP_TYPE_NAMES` in `mso5202d.py`; per-channel enums reuse the main
trigger name maps.)

**Vertical (CHx) menu enums** — mapped by stepping the CH1 menu
(`mso5202d-ch1-menu.pcapng`, menu id `[CONTROL-MENUID]` = **1**) and confirmed
on CH2 (`mso5202d-ch2-menu.pcapng`, menu id **2**, identical layout & values):
- `[VERT-CHx-COUP]`: `0` = **DC**, `1` = **AC**, `2` = **GND** (channel input
  coupling — **not** the same enum as `[TRIG-COUP]`).
- `[VERT-CHx-20MHZ]`: `0` = **Full** bandwidth, `1` = **20 MHz** limit.
- `[VERT-CHx-FINE]`: `0` = **Coarse**, `1` = **Fine** (V/div resolution).
- `[VERT-CHx-PROBE]`: `0` = **1×**, `1` = **10×**, `2` = **100×**, `3` = **1000×**.
- `[VERT-CHx-RPHASE]`: `0` = **Off**, `1` = **On** — the **Invert** function.
  Inverting also flips the sign of the trigger level (observed `TRIG-VPOS`
  81→55, `TRIG-LEVEL-mV` +2600→−2600) since the trigger follows the inverted
  trace — but **only for the trigger-source channel**: inverting CH2 (with the
  trigger on CH1) left the level unchanged, confirming the source-binding.

(`VERT_COUP_NAMES` / `VERT_BW_NAMES` / `VERT_FINE_NAMES` / `VERT_PROBE_NAMES` /
`VERT_INVERT_NAMES` in `mso5202d.py`.)

**Acquire-menu enums** — mapped by stepping the Acquire menu
(`mso5202d-acquire.pcapng`, menu id `[CONTROL-MENUID]` = **17**):
- `[ACQURIE-TYPE]`: `0` = **Realtime**, `1` = **Equivalent-time**.
- `[ACQURIE-MODE]`: `0` = **Normal**, `1` = **Peak Detect**, `2` = **Average**.
- `[ACQURIE-AVG-CNT]`: index → averages, `0`=4 `1`=8 `2`=16 `3`=32 `4`=64
  `5`=128 (count = 4·2ⁿ).
- `[ACQURIE-STORE-DEPTH]`: record length, **gapped codes** (unavailable/greyed
  depths still occupy enum slots): `0` = **4K**, `4` = **40K**, `6` = **512K**,
  `7` = **1M**. 1M is single-channel only (captured with one channel on;
  `mso5202d-acquire-1m.pcapng`); dual-channel maxes at 512K, matching the manual.
  The gaps (1/2/3/5) are greyed-out depths (e.g. 20K).

(`ACQ_TYPE_NAMES` / `ACQ_MODE_NAMES` / `ACQ_AVG_COUNTS` / `ACQ_DEPTH_NAMES` in
`mso5202d.py`.)

**Display-menu enums** — mapped by stepping the Display menu
(`mso5202d-display.pcapng`; two pages: `[CONTROL-MENUID]` **4** =
Type/Persist/Contrast, **36** = Grid/Format):
- `[DISPLAY-MODE]`: `0` = **Vectors**, `1` = **Dots** (waveform draw type).
- `[DISPLAY-FORMAT]`: `0` = **XT** (YT), `1` = **XY**.
- `[DISPLAY-GRID-KIND]`: `0` = **Off**, `1` = **Dotted**, `2` = **RealLine**
  (grid style; the Off/Dotted/RealLine↔0/1/2 order is inferred from cycle order).
- `[DISPLAY-GRID-BRIGHT]`: grid intensity **0…15** (max = `[DISPLAY-MAXGRID-BRIGHT]`=15).
- `[DISPLAY-CONTRAST]`: waveform/display intensity **0…15** (max =
  `[DISPLAY-MAXCONTRAST]`=15).
- `[DISPLAY-PERSIST]`: persistence, **gapped codes** `0`=Auto, `2`=0.2s,
  `4`=0.4s, `8`=0.8s, `10`=1.0s, `11`=2.0s, `13`=4.0s, `17`=8.0s, `19`=Infinity.

Not captured in the blob: the **Refresh rate** control (Auto/30/40/50 fps —
no field changed when adjusted) and the second of the two 0…15 waveform
controls (only `DISPLAY-CONTRAST` moved; "wave intensity" vs "contrast" not
distinctly separable here). (`DISPLAY_MODE_NAMES` / `DISPLAY_FORMAT_NAMES` /
`DISPLAY_GRID_NAMES` / `DISPLAY_PERSIST_NAMES` in `mso5202d.py`.)

**Math-menu enums** — mapped by stepping the Math menu (`mso5202d-math.pcapng`;
menu ids `[CONTROL-MENUID]` **41** = operations, **16** = FFT page 1
(source/window), **56** = FFT page 2 (factor/scale)):
- `[MATH-MODE]`: `0`=CH1+CH2, `1`=CH1−CH2, `2`=CH2−CH1, `3`=CH1×CH2,
  `4`=CH1/CH2, `5`=CH2/CH1, `6`=**FFT**. Selecting FFT sets
  `[DISPLAY-FORMAT]` = **2** (so FORMAT is 0=XT, 1=XY, **2=FFT**).
- `[MATH-FFT-SRC]`: `0`=CH1, `1`=CH2.
- `[MATH-FFT-WIN]`: `0`=Hanning, `1`=Flattop, `2`=Rectangular (verified);
  `3`=Bartlett, `4`=Blackman (inferred — only 0–2 were swept).
- `[MATH-FFT-FACTOR]`: FFT (horizontal) zoom `0`=×1, `1`=×2, `2`=×5, `3`=×10.
- `[MATH-FFT-DB]`: FFT vertical **dB/div** scale: `0`=1dB, `1`=2dB, `2`=5dB,
  `3`=10dB, `4`=20dB.
- In FFT mode the **frequency axis tracks the timebase/sample rate** — at the
  slowest sweep (5.00 S/s) the resolution bottoms out at **250 mHz**.

(`MATH_MODE_NAMES` / `MATH_FFT_SRC_NAMES` / `MATH_FFT_WIN_NAMES` /
`MATH_FFT_FACTOR_NAMES` in `mso5202d.py`.)

**Cursor menu — NOT in the settings blob** (`mso5202d-cursor.pcapng`): stepping
cursor Type (Off/Time/Voltage/Track), Source (CH1/CH2/Math/RefA/RefB/LA), the
S/E select and the cursor positions produced **zero** changes in the 213-byte
state — consistent with there being no `CURSOR-*` param in `/protocol.inf`.
Only the cursor menu id is observable: `[CONTROL-MENUID]` = **15**. (Like the
horizontal marks and Display refresh-rate, cursor state lives outside the polled
blob and can't be read back over this protocol.) The same capture caught
`[MATH-DISP]` = **0/1 math on/off** and the Math menu id **41** (Math otherwise
not yet swept).

**AUTOSET** (front-panel button, `mso5202d-autoset.pcapng`) is a **compound
reconfigure**, not a single field: it re-scales the timebase
(`[HORIZ-TB]`/`[HORIZ-WIN-TB]`), can set `[TRIG-EDGE-SLOPE]`, and cycles
`[TRIG-STATE]` (stop → auto → trig'd) as it re-arms. **Caveat:** while Autoset
runs the scope goes briefly unresponsive — the settings poll times out for a
second or two (a driver should tolerate this), so some fast intermediate changes
(e.g. the vertical V/div the manual says Autoset also adjusts) were not captured
cleanly. Autoset thus wasn't mapped to a single command; it's a firmware macro
that ends in a normal settings state.

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
  signed **1/25-div** units (`level_V = (VPOS − POS_src) × vdiv/25`,
  scope-calibrated), 8-byte time
  fields in **picoseconds** (incl. `[HORIZ-TRIGTIME]` = horizontal delay), and
  `[TRIG-FREQUENCY]` in mHz.
- Waveform handshake and 8-bit sample format (1 kHz cal square wave confirmed).

- `[HORIZ-TB]`/`[HORIZ-WIN-TB]` index → time/div table (2 ns…40 s, **2-4-8**
  sequence, 32 steps; WIN-TB = knob, HORIZ-TB clamps at 200 ns — §6, proven by
  the time/div sweep capture + on-screen readings).

- **2-channel readout**: acquire value byte selects the channel (`02 01 <ch>`,
  0 = CH1, 1 = CH2) — verified square-vs-flat on hardware (§5).

- **Horizontal sample rate**: 200 samples/div → sample interval `TDIV/200`,
  block = 19.2 div — matches the vendor manual and our cal-signal cycle counts
  to the digit (§5). X axis can now be plotted in real seconds.

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
   (1/25 div, scope-calibrated) and the ps time fields (§6). **Remaining follow-up of the same
   shape:** toggle coupling / trigger-mode / acquire menus to enumerate the
   enum-coded fields.
2. **Dump more scope files** via selector `0x10`: try `/system.inf`, `/cal.inf`,
   `/calibration.inf`, `/factory.inf`, a directory listing, etc. One of these
   should hold the **counts→volts / index→"1 V/div" / timebase→seconds** tables.
   (We can now do this directly with `mso5202d.py`.)
3. ~~**Crack 2-channel readout**~~ **DONE (2026-07-08)** — the acquire value
   byte is the channel: `02 01 00` = CH1, `02 01 01` = CH2 (§5). Also
   established: off-screen positioning clips samples to the rails / returns
   rail-to-rail blocks (§5, `mso5202d-ch1-vpos.pcapng`). **Still open — vertical
   amplitude** depends on the trace's distance from screen centre (full-swing
   near the 0 axis, compressed far from it — §5); model it with a POS sweep
   logging amplitude+baseline, then a V/div sweep at fixed centre position.
   **Also open — inter-channel phase** is lost (sequential acquires, no shared
   trigger — §5); look for a single-acquire both-channels command. And: what
   param `0x12` does.
4. **Host-side control:** find the command that presses a `/keyprotocol.inf` key
   (likely another selector with a key id), enabling PC control of V/div, timebase,
   trigger, autoset, single-seq, etc. A fresh capture of the app *changing settings
   from the PC* (if it supports it) would reveal this directly.
5. ~~**Sample rate**~~ **DONE (2026-07-08)** — **200 samples/div** exactly
   (sample interval = `TDIV/200`), so the 3840-sample block spans 19.2 div.
   Confirmed against the vendor manual's "sample interval = s/div ÷ 200" and
   our cal-cycle counts to the digit; X axis now plots in real seconds (§5).

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

## Appendix D — settings-blob field datasheet (offsets, units, enums)

The settings poll (`0x01`) returns `53 <len> 81 <213 param bytes> <ck>`. The 213
bytes are the `/protocol.inf` list (Appendix A) starting at **raw frame
offset 4** (right after the `0x81` echo). Multi-byte fields are little-endian;
positions/levels are signed. `decode_settings()` in `mso5202d.py` decodes all of
these. Offsets below are into the raw frame (`53 xx xx 81 …`).

**Units summary:** position/level fields (`VERT-CHx-POS`, `TRIG-VPOS`) are signed
**1/25-division** units (25 counts/div; ±200 = ±8 div) — scope-calibrated §6;
all 8-byte time fields are **picoseconds**; `TRIG-FREQUENCY` is **mHz**;
`*-VB` are V/div indices (`VB_TO_MV`); `HORIZ-*TB` are timebase indices
(`TB_TO_NS`, 2-4-8 sequence, §6).

```
off  w  field                       decoded meaning / enum (blank = raw value, meaning TBD)
--- VERTICAL, CH1 ---
4    1  VERT-CH1-DISP               0/1 channel displayed
5    1  VERT-CH1-VB                 V/div index -> VB_TO_MV (0=2mV..10=5V; 10V/div re-uses 0)
6    1  VERT-CH1-COUP               0=DC 1=AC 2=GND (channel coupling; NOT trigger coup)
7    1  VERT-CH1-20MHZ              0=Full 1=20 MHz BW limit
8    1  VERT-CH1-FINE               0=Coarse 1=Fine (V/div resolution)
9    1  VERT-CH1-PROBE              0=1x 1=10x 2=100x 3=1000x
10   1  VERT-CH1-RPHASE             0=Off 1=On (INVERT; also flips trigger-level sign)
11   1  VERT-CH1-CNT-FINE           fine-gain counter (unmapped)
12   2  VERT-CH1-POS                signed; vertical position, 1/25 div
--- VERTICAL, CH2 (same layout) ---
14   1  VERT-CH2-DISP               0/1 channel displayed
15   1  VERT-CH2-VB                 V/div index -> VB_TO_MV
16   1  VERT-CH2-COUP               0=DC 1=AC 2=GND
17   1  VERT-CH2-20MHZ              0=Full 1=20 MHz BW limit
18   1  VERT-CH2-FINE               0=Coarse 1=Fine
19   1  VERT-CH2-PROBE              0=1x 1=10x 2=100x 3=1000x
20   1  VERT-CH2-RPHASE             (unmapped)
21   1  VERT-CH2-CNT-FINE           (unmapped)
22   2  VERT-CH2-POS                signed; vertical position, 1/25 div
--- TRIGGER (main) ---
24   1  TRIG-STATE                  0=STOP 1=WAIT(Normal) 2=AUTO 3=TRIG'D 4=SCAN 5=SINGLE 6=ARMING
25   1  TRIG-TYPE                   0=Edge 1=Video 2=Pulse 3=Slope 4=Overtime 5=Alter
26   1  TRIG-SRC                    0=CH1 1=CH2 2=EXT 3=EXT/5 4=AC-line
27   1  TRIG-MODE                   0=Auto 1=Normal (mirrors TRIG-SWAP-CHx-MODE)
28   1  TRIG-COUP                   0=DC 1=AC 2=NoiseRej 3=HFRej 4=LFRej
29   2  TRIG-VPOS                   signed; trigger marker pos, 1/25 div.
                                    level_V = (TRIG-VPOS - POS_src) * vdiv / 25
31   8  TRIG-FREQUENCY              frequency counter, mHz (0 = not triggering)
39   8  TRIG-HOLDTIME-MIN           ps (= 100 ns, holdoff lower limit)
47   8  TRIG-HOLDTIME-MAX           ps (= 10 s, holdoff upper limit)
55   8  TRIG-HOLDTIME               ps; live holdoff (HORIZONTAL menu > F4; knob-push resets to 100 ns)
63   1  TRIG-EDGE-SLOPE             0=Rising 1=Falling
--- TRIGGER: Video sub-params ---
64   1  TRIG-VIDEO-NEG              0=Normal 1=Inverted
65   1  TRIG-VIDEO-PAL             0=NTSC 1=PAL/SECAM
66   1  TRIG-VIDEO-SYN             0=AllLines 1=LineNum 2=OddField 3=EvenField 4=AllFields
67   2  TRIG-VIDEO-LINE            line number (1..525 NTSC / 1..625 PAL), used when SYN=LineNum
--- TRIGGER: Pulse sub-params ---
69   1  TRIG-PULSE-NEG             0=Positive 1=Negative pulse
70   1  TRIG-PULSE-WHEN            0='=' 1='≠' 2='>' 3='<' (== TRIG-SLOPE-WHEN)
71   8  TRIG-PULSE-TIME            ps; pulse width 20 ns .. 10 s (default 500000 = 500 ns)
--- TRIGGER: Slope sub-params ---
79   1  TRIG-SLOPE-SET             0=Positive slope 1=Negative
80   1  TRIG-SLOPE-WIN             0=V1(upper) 1=V2(lower) 2=Both (knob-adjust select)
81   1  TRIG-SLOPE-WHEN            0='=' 1='≠' 2='>' 3='<'
82   2  TRIG-SLOPE-V1              signed; upper slope threshold, 1/25 div (volts = (V1-POS)*vdiv/25, scope-verified)
84   2  TRIG-SLOPE-V2              signed; lower slope threshold, 1/25 div
86   8  TRIG-SLOPE-TIME            ps (20 ns .. 10 s)
--- TRIGGER: Alter/Swap per-channel blocks (CH1 @94, CH2 @122) ---
; In Alter mode each channel has its own trigger config here; sub-params reuse
; the main-trigger enums. TRIG-SRC alternates CH1<->CH2 as the scope switches.
94   1  TRIG-SWAP-CH1-TYPE          0=Edge 1=Video 2=Pulse 3=Overtime (4-value, no Slope/Alter)
95   1  TRIG-SWAP-CH1-MODE
96   1  TRIG-SWAP-CH1-COUP
97   1  TRIG-SWAP-CH1-EDGE-SLOPE
98   1  TRIG-SWAP-CH1-VIDEO-NEG
99   1  TRIG-SWAP-CH1-VIDEO-PAL
100  1  TRIG-SWAP-CH1-VIDEO-SYN
101  2  TRIG-SWAP-CH1-VIDEO-LINE
103  1  TRIG-SWAP-CH1-PULSE-NEG
104  1  TRIG-SWAP-CH1-PULSE-WHEN
105  8  TRIG-SWAP-CH1-PULSE-TIME    ps
113  1  TRIG-SWAP-CH1-OVERTIME-NEG
114  8  TRIG-SWAP-CH1-OVERTIME-TIME ps
122  1  TRIG-SWAP-CH2-TYPE          (CH2 block, same layout as CH1 @94..121)
123  1  TRIG-SWAP-CH2-MODE
124  1  TRIG-SWAP-CH2-COUP
125  1  TRIG-SWAP-CH2-EDGE-SLOPE
126  1  TRIG-SWAP-CH2-VIDEO-NEG
127  1  TRIG-SWAP-CH2-VIDEO-PAL
128  1  TRIG-SWAP-CH2-VIDEO-SYN
129  2  TRIG-SWAP-CH2-VIDEO-LINE
131  1  TRIG-SWAP-CH2-PULSE-NEG
132  1  TRIG-SWAP-CH2-PULSE-WHEN
133  8  TRIG-SWAP-CH2-PULSE-TIME    ps
141  1  TRIG-SWAP-CH2-OVERTIME-NEG
142  8  TRIG-SWAP-CH2-OVERTIME-TIME ps
--- TRIGGER: Overtime sub-params ---
150  1  TRIG-OVERTIME-NEG          0=Positive 1=Negative
151  8  TRIG-OVERTIME-TIME         ps; overtime 20 ns .. 10 s
--- HORIZONTAL ---
159  1  HORIZ-TB                   acquisition timebase index -> TB_TO_NS (clamps at 6 = 200 ns)
160  1  HORIZ-WIN-TB               knob timebase index 0..31 -> TB_TO_NS (2-4-8 seq)
161  1  HORIZ-WIN-STATE            window/zoom state (never changed via Horizontal menu; needs dual-window engaged?)
162  8  HORIZ-TRIGTIME             SIGNED ps; horizontal position/delay (goes negative = post-trigger)
--- MATH ---
170  1  MATH-DISP                  0/1 math on/off (Math menu = CONTROL-MENUID 41)
171  1  MATH-MODE                  0=CH1+CH2 1=CH1-CH2 2=CH2-CH1 3=CH1*CH2 4=CH1/CH2 5=CH2/CH1 6=FFT
172  1  MATH-FFT-SRC               0=CH1 1=CH2
173  1  MATH-FFT-WIN               0=Hanning 1=Flattop 2=Rectangular (3=Bartlett 4=Blackman, inferred)
174  1  MATH-FFT-FACTOR            FFT zoom 0=x1 1=x2 2=x5 3=x10
175  1  MATH-FFT-DB                FFT vertical dB/div: 0=1dB 1=2dB 2=5dB 3=10dB 4=20dB
--- DISPLAY ---
176  1  DISPLAY-MODE               0=Vectors 1=Dots (draw type)
177  1  DISPLAY-PERSIST            0=Auto 2=0.2s 4=0.4s 8=0.8s 10=1.0s 11=2.0s 13=4.0s 17=8.0s 19=Infinity
178  1  DISPLAY-FORMAT             0=XT 1=XY 2=FFT (set when MATH-MODE=FFT)
179  1  DISPLAY-CONTRAST           waveform/display intensity 0..15
180  1  DISPLAY-MAXCONTRAST        max contrast (=15)
181  1  DISPLAY-GRID-KIND          0=Off 1=Dotted 2=RealLine (order inferred)
182  1  DISPLAY-GRID-BRIGHT        grid intensity 0..15
183  1  DISPLAY-MAXGRID-BRIGHT     max grid brightness (=15)
--- ACQUIRE ---
184  1  ACQURIE-MODE               0=Normal 1=Peak 2=Average
185  1  ACQURIE-AVG-CNT            avg index: 0=4 1=8 2=16 3=32 4=64 5=128 (count=4*2^n)
186  1  ACQURIE-TYPE               0=Realtime 1=Equivalent-time
187  1  ACQURIE-STORE-DEPTH        record length: 0=4K 4=40K 6=512K 7=1M (1M single-ch only; gaps=greyed depths)
--- MEASURE (8 slots, each: SRC then item id) ---
188  1  MEASURE-ITEM1-SRC          measurement source (0=CH1,1=CH2 seen)
189  1  MEASURE-ITEM1              measurement id (unmapped)
190..203                          ITEM2..ITEM8 (SRC,id) pairs, same layout
--- CONTROL (menu/UI state) ---
204  1  CONTROL-TYPE               always 0 observed
205  1  CONTROL-MENUID             current menu id (see table below)
206  1  CONTROL-DISP-MENU          0/1 menu displayed on screen
--- LOGIC ANALYZER ---
207  1  LA-SWI                     0/1 logic-analyzer on/off (LA menu = CONTROL-MENUID 61)
208  2  LA-CHANNEL-STATE           per-bit D0..D15 enable mask (=255 seen)
210  1  LA-CURRENT-CHANNEL         (unmapped)
211  1  LA-D7-D0-THRESHOLD-TYPE
212  1  LA-D15-D8-THRESHOLD-TYPE
213  2  LA-D7-D0-USER-THRESHOLD-VOLT   signed
215  2  LA-D15-D8-USER-THRESHOLD-VOLT  signed
(217 = checksum)
```

### Enum tables (mapped so far)

| field | value → meaning |
|---|---|
| `TRIG-STATE` | 0 STOP · 1 WAIT (Normal, no trig) · 2 AUTO (no trig) · 3 TRIG'D · 4 SCAN/roll · 5 SINGLE (armed) · 6 ARMING flicker |
| `TRIG-TYPE` | 0 Edge · 1 Video · 2 Pulse · 3 Slope · 4 Overtime · 5 Alter (swap; alternates `TRIG-SRC` CH1↔CH2) |
| `TRIG-SRC` | 0 CH1 · 1 CH2 · 2 EXT · 3 EXT/5 · 4 AC line |
| `TRIG-MODE` | 0 Auto · 1 Normal (mirrors into `TRIG-SWAP-CHx-MODE`) |
| `TRIG-EDGE-SLOPE` | 0 Rising · 1 Falling |
| `TRIG-COUP` | 0 DC · 1 AC · 2 Noise Reject · 3 HF Reject · 4 LF Reject |
| `TRIG-VIDEO-NEG` | 0 Normal · 1 Inverted |
| `TRIG-VIDEO-PAL` | 0 NTSC · 1 PAL/SECAM |
| `TRIG-VIDEO-SYN` | 0 All Lines · 1 Line Num · 2 Odd Field · 3 Even Field · 4 All Fields |
| `TRIG-VIDEO-LINE` | line number 1…525 (NTSC) / 1…625 (PAL); used when SYN = Line Num |
| `TRIG-SLOPE-SET` | 0 Positive slope · 1 Negative |
| `TRIG-SLOPE-WIN` | 0 V1 (upper) · 1 V2 (lower) · 2 Both |
| `TRIG-SLOPE-WHEN` / `TRIG-PULSE-WHEN` | 0 = · 1 ≠ · 2 > · 3 < (same enum, confirmed on both) |
| `TRIG-PULSE-NEG` | 0 Positive · 1 Negative pulse |
| `TRIG-OVERTIME-NEG` | 0 Positive · 1 Negative |
| `TRIG-SWAP-CHx-TYPE` | 0 Edge · 1 Video · 2 Pulse · 3 Overtime (per-channel type in Alter mode; 4-value, no Slope/Alter) |
| `VERT-CHx-COUP` | 0 DC · 1 AC · 2 GND (channel coupling ≠ trigger coupling) |
| `VERT-CHx-20MHZ` | 0 Full · 1 20 MHz limit |
| `VERT-CHx-FINE` | 0 Coarse · 1 Fine |
| `VERT-CHx-PROBE` | 0 1× · 1 10× · 2 100× · 3 1000× |
| `VERT-CHx-RPHASE` | 0 Off · 1 On (Invert) |
| `ACQURIE-TYPE` | 0 Realtime · 1 Equivalent-time |
| `ACQURIE-MODE` | 0 Normal · 1 Peak Detect · 2 Average |
| `ACQURIE-AVG-CNT` | 0=4 · 1=8 · 2=16 · 3=32 · 4=64 · 5=128 (count = 4·2ⁿ) |
| `ACQURIE-STORE-DEPTH` | 0=4K · 4=40K · 6=512K · 7=1M (1M single-ch; gaps 1/2/3/5 = greyed depths) |
| `MATH-MODE` | 0 CH1+CH2 · 1 CH1−CH2 · 2 CH2−CH1 · 3 CH1×CH2 · 4 CH1/CH2 · 5 CH2/CH1 · 6 FFT |
| `MATH-FFT-SRC` | 0 CH1 · 1 CH2 |
| `MATH-FFT-WIN` | 0 Hanning · 1 Flattop · 2 Rectangular · 3 Bartlett · 4 Blackman (3/4 inferred) |
| `MATH-FFT-FACTOR` | 0 ×1 · 1 ×2 · 2 ×5 · 3 ×10 (FFT zoom) |
| `MATH-FFT-DB` | FFT dB/div: 0 1dB · 1 2dB · 2 5dB · 3 10dB · 4 20dB |
| `DISPLAY-MODE` | 0 Vectors · 1 Dots |
| `DISPLAY-FORMAT` | 0 XT · 1 XY · 2 FFT |
| `DISPLAY-GRID-KIND` | 0 Off · 1 Dotted · 2 RealLine |
| `DISPLAY-PERSIST` | 0 Auto · 2 0.2s · 4 0.4s · 8 0.8s · 10 1.0s · 11 2.0s · 13 4.0s · 17 8.0s · 19 Infinity |
| `DISPLAY-CONTRAST` / `-GRID-BRIGHT` | 0…15 intensity (max = the `-MAX*` fields = 15) |
| `VERT-CHx-VB` | V/div: 0=2mV 1=5mV 2=10 3=20 4=50 5=100 6=200 7=500mV 8=1V 9=2V 10=5V (10V/div → 0) |
| `HORIZ-*TB` | time/div index, 2-4-8 sequence 2 ns…40 s (`TB_TO_NS`); WIN-TB = knob 0..31, HORIZ-TB clamps at 6 (200 ns) |

Still-unmapped enums (need targeted captures): `MATH-*`, `DISPLAY-*`, the
`MEASURE-ITEM*` ids, `LA-*` thresholds, and the EXT-source trigger level in
volts.

### `CONTROL-MENUID` — on-screen menu id (partial, mapped by context)

| id | menu |
|---|---|
| 1 | **CH1** vertical menu |
| 2 | **CH2** vertical menu |
| 3 | Horizontal menu, **page 1** (window ctrl / marks) |
| 4 | Display menu (Type / Persist / Contrast page) |
| 5 | Trigger → Edge submenu |
| 6 | Trigger → Pulse submenu, **page 1** |
| 7 | Trigger → Pulse submenu, **page 2** (When / Time) |
| 8 | Trigger → Video submenu |
| 10 | default / no active menu (vendor-app baseline) |
| 11 | Trigger menu (level/base, Edge default) |
| 15 | Cursor menu (cursor state is **not** in the settings blob) |
| 16 | Math → FFT submenu, **page 1** (source / window) |
| 17 | **Acquire** menu |
| 22 | Trigger → Slope submenu, **page 1** |
| 23 | Trigger → Slope submenu, **page 2** (V1/V2 / When / Time) |
| 24 | Trigger → Alter submenu (base) |
| 26 / 27 / 28 / 29 | Alter → **CH1** Edge / Pulse / Video / Overtime |
| 30 / 31 / 32 / 33 | Alter → **CH2** Edge / Pulse / Video / Overtime |
| 38 | Trigger → Overtime submenu, **page 1** |
| 39 | Trigger → Overtime submenu, **page 2** (Coupling) |
| 36 | Display menu (Grid / Format page) |
| 40 | Horizontal menu, **page 2** (holdoff / play-stop / coarse-fine) |
| 41 | Math menu (operations; `[MATH-DISP]` = math on/off) |
| 56 | Math → FFT submenu, **page 2** (zoom factor / vertical scale) |
| 61 | Logic Analyzer menu (`[LA-SWI]` = LA on/off) |

Note some menus set `[CONTROL-MENUID]` but keep it constant while open (so it
shows in the value, not as a change) — e.g. CH1=1, CH2=2, Acquire=17 stayed put
throughout their captures; only `[CONTROL-DISP-MENU]` toggled 0↔1.

Multi-page trigger submenus use **consecutive ids** for page 1 / page 2
(Pulse 6/7, Slope 22/23, Overtime 38/39). `CONTROL-DISP-MENU` = 1 while a menu is shown, 0 when
closed. `CONTROL-TYPE` stayed 0 in every capture. (`MENU_NAMES` in
`mso5202d.py`.) More menu ids (Acquire, Display, Measure, Math, Utility,
Save/Recall, LA…) remain to be mapped by opening each menu.
