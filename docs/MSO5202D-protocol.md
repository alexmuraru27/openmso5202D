# Hantek MSO5202D — USB protocol reference

**Status: transport, framing, the core handshakes and host-side control are
reverse-engineered and verified against real hardware (Linux/pyusb).** A working
driver (`mso5202d.py`) connects, reads the scope's self-description, decodes live
settings, and captures waveforms (a 1 kHz cal square wave was confirmed). The
PC→scope control path (write settings / key events / screen grab) is decoded in
**Appendix F**. This document is intended to be **self-contained** — if all other
context is lost, everything needed to re-derive and re-implement the driver is here.

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
| `0x02` | `01` `<ch>` | **acquire channel** `<ch>` → waveform (§5). `00`=CH1 `01`=CH2 `02`=Math `05`=LA. `53 04 00 02 01 00 5a` |
| `0x11` | 213-byte block | **WRITE settings** — push the whole `/protocol.inf` block; sets any field (host-side control, **Appendix F.1**). |
| `0x13` | `<keyid>` `<state>` | **key event** — press a front-panel key (`keyid` = `/keyprotocol.inf` index; **Appendix F.2**). |
| `0x20` | (none) | **screen grab** — scope streams its RGB565 framebuffer (**Appendix F.3**). |

SET form is `selector | vlen | value…` (the selector doubles as the param id).
Read-file form is `0x10 | 0x00 | <path>`. **Host-side control** (`0x11`/`0x13`/`0x20`)
is decoded in full in **Appendix F**.

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

Two files are known; there are almost certainly more (calibration, system — the
counts→volts calibration table likely lives in one of these; still unmapped).
Full contents are in the appendices.

- **`/protocol.inf`** (≈3.6 KB) — an ordered list of **every setting parameter**
  the scope exposes, each with its **byte width** in the settings blob. First line
  `[TOTAL] 213` = total parameter bytes. See **Appendix A** for the complete list.
- **`/keyprotocol.inf`** (≈0.9 KB) — the list of **front-panel keys**
  (`[VT-CH1-VBSUB-KEY]`, `[HZ-TBADD-KEY]`, `[CT-AUTOSET-KEY]`, …). See
  **Appendix B**. These are exactly the keys the host presses via the **`0x13`
  key-event command** — **confirmed** from the vendor app (**Appendix F.2**).

---

## 5. Handshake: waveform acquisition

Per refresh the app runs (verified on hardware; samples confirmed as a 1 kHz cal
square wave):

```
OUT  53 04 00 12 01 00        ; param 0x12 = 0 (NOT channel select; see below)
                                                      -> IN 53 04 00 92 01 00        (ack, subtype 01)
OUT  53 04 00 02 01 <ch>      ; acquire CHANNEL <ch> — see channel-code map below
                                                      -> IN 53 07 00 82 00 00 00 0f 00  (subtype 00 = size; 0x0F00 = 3840)
                                                      -> IN 53 04 0f 82 01 <ch> <samples>  (subtype 01 = data)
                                                      -> IN 53 04 00 82 02 <ch>      (subtype 02 = end-marker)
```

- **Acquire channel-code map (`<ch>` = value byte of `02 01 <ch>`) — SOLVED
  (2026-07-09):** `00` = **CH1**, `01` = **CH2**, `02` = **Math**, `05` = **Logic
  Analyzer**. Codes `03`/`04` are not usable channels (`03` replies empty; `04`
  returns a dual-analog block) and `06`+ are invalid — they get no reply and
  **desync the link** (avoid probing them). The data frame echoes the code in its
  3rd byte (`82 01 <ch>`), so the response is self-identifying.
- **Samples: analog channels (CH1/CH2/Math) are 8-bit unsigned, 1 byte each**,
  **always 3840 per block = the on-screen display window** (19.2 div). The waveform
  frame payload is `82 01 <ch> <3840 bytes>`.
- **Logic Analyzer (`02 01 05`) — SOLVED (2026-07-09):** returns the same 3840
  samples but **2 bytes per sample** (little-endian **16-bit word**, 7680 bytes
  total), because it packs all 16 digital channels into each sample: **bit N =
  channel D(N)** (D0 = LSB … D15 = MSB, same convention as `LA-CHANNEL-STATE`).
  So `word = raw[2i] | (raw[2i+1] << 8)` and `Dn(i) = (word >> n) & 1`. The size
  frame still reports `0x0F00` = 3840 *samples* (not bytes). LA must be on
  (`LA-SWI` = 1) or the read returns just an end-marker. Decoded by `read_la()` /
  `decode_la()` in `mso5202d.py`.
- **⚠ …but `02 01 05` is NOT a usable read — it is a half-wired firmware path
  (2026-07-09).** The *frame format* above is correct, but the firmware does not
  actually serve live LA samples over USB:
  - The returned payload is **2-state garbage** at most timebases (only the words
    `0x007f`/`0xff81` toggling at one ~62.5 Hz rate — no per-channel frequency
    gradient), and its value is even influenced by the `0x12` command. It becomes
    *partially* coherent only at slow timebases (e.g. 40 ms/div: 15 distinct
    words), but never matches the scope's own display.
  - **Reading it corrupts the scope's on-screen LA display** — the instrument's
    own D0–D15 traces get overwritten with the 2-state pattern while we read.
  - No observed vendor-app USB traffic ever issues `02 01 05`; the vendor's
    virtual panel instead displays LA via the **`0x20` framebuffer** (the scope's
    rendered screen, which already contains the firmware-drawn LA rows) — the
    safe, correct way to view LA over USB. `read_la()` is retained for RE only;
    the live viewer keeps it disabled
    (`LA_READ_ENABLED = False`). See MSO5202D-rendering.md §6.
- **The `0x02` acquire reads only the screen buffer, NOT deep memory —
  CONFIRMED (2026-07-09).** With `ACQURIE-STORE-DEPTH` at the default it returns
  the 3840-sample screen; set to **40K it stops responding entirely** (`0x02`
  gets no reply — read returns 0 bytes, every retry times out). So the deep
  record (40K/512K/1M) is **not reachable through this command**.
- **The vendor app has no deep read either — VERIFIED against its own USB
  traffic (2026-07-09).** A capture of the vendor Windows application performing
  a live acquisition shows its refresh loop is byte-for-byte our own: per frame
  it polls settings (`01`→`81`, 218 B), toggles the `0x12` param (`12 01 00`↔
  `12 01 01`→`92`), then issues the same `02 01 <ch>` acquire and gets back the
  identical **3-frame** reply — size (`82 00`, reports `0x00000f00` = 3840), data
  (`82 01 <ch>` + **3840** sample bytes = a 3847-byte frame), end-marker
  (`82 02`). **The largest single transfer the app ever makes is 3847 bytes
  (= the 3840-sample screen block); it never reads more.** It does not use any
  larger or alternate read command, so the deep record is simply **not exposed
  over the USB host link at all** — consistent with the panel refusing to serve
  40K/512K over USB even from its own vendor app. Deep single-shot capture, if it
  exists, lives only in the on-instrument *Save waveform → USB stick* path (file
  export), not as a host-issued read. Keep store depth at the screen size for
  live readout; there is no deep-read command left to find over USB.
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
- **Multi-channel readout — SOLVED (2026-07-08, extended 2026-07-09):** the
  channel is selected by the **acquire value byte** `02 01 <ch>`: `00` = CH1,
  `01` = CH2, `02` = Math, `05` = **LA** (16-bit words, 2 B/sample — see the
  acquire-code map above). Verified on hardware with CH2's probe disconnected
  (CH1 returned the square wave, CH2 its flat line);
  `../captures/mso5202d-2ch-readout.pcapng` records 6 alternating CH1/CH2 acquire
  pairs with their distinct 3840-sample responses. The `0x12` param is **not** a
  channel select — early tests varying it returned identical data because it does
  something else entirely
  (the vendor app toggles it `1` → `0` around every refresh; run/hold?).
  Values `12 01 02`/`03` make the next acquire return nothing. Note the vendor
  captures (`session1/2`) were taken with **CH2 display off**, which is why
  they never showed a CH2 fetch.
- **Vertical byte encoding — SOLVED (2026-07-09).** A controlled single-channel
  `VERT-CH1-POS` sweep (one channel, one V/div, one 1 kHz/5 V cal, moving the
  trace bottom-off-screen → top-off-screen) with **POS read from the same frames**
  pins the encoding exactly. Each sample byte is:

  **`byte = (VERT-CHx-POS + 16 + signal) mod 256`**

  where `VERT-CHx-POS` is the channel's position (1/25-div units) and `signal` is
  the AC waveform in counts (**25 counts/division**, so ≈28 counts p-p for a 1-div
  cal). Consequences — and the two bugs they cause a naïve decoder:
  - **Baseline** `= (POS + 16) mod 256`, slope **exactly 1 byte per POS unit**
    (verified: POS 18→byte 34, 50→66, 95→111; −57→213, −90→179 …).
  - **Reversed sense:** the byte *rises* as the trace moves **up** (POS up). A
    decoder that does `128 − byte` moves the trace the wrong way ("raise → app
    descends").
  - **8-bit wrap:** as the trace nears screen centre the baseline nears the byte
    boundary (0/256), so the AC signal wraps around it → a **rail-to-rail "hash"**
    block (~500-sample runs pinned to `≈0x08`/`≈0xF2` alternating with mid). This
    is *not* a bad frame — it is the real signal, folded across the 8-bit edge.
  - **Off-screen / parked:** far past ±4 div the block **flat-lines near mid-code
    (~129)** — no data.

  **Correct decode** (undoes both the reverse and the wrap; recovers a clean trace
  at every position, centre included):
  ```
  base   = (POS + 16) & 0xFF
  signal = ((byte − base + 128) mod 256) − 128      # AC counts, unwrapped
  y_div  = (POS + signal) / 25                       # divisions, up = positive
  ```
  This is what `mso5202d_plot.py` / `MSO5202D-rendering.md` implement. (Absolute
  `counts→volts` is still uncalibrated — 25 counts/div is scale; the viewer shows
  divisions, each = that channel's V/div.)
- **OPEN — inter-channel PHASE is not preserved.** CH1 and CH2 are fetched as
  two separate acquires (~100 ms apart) with no shared trigger lock, so their
  returned phase is uncorrelated: the CH1→CH2 first-rising-edge offset jittered
  66/89/30/94 samples across reads (one 1 kHz period = 500 samples at
  400 µs/div). On the scope the two channels are sampled simultaneously and look
  in-phase; our plotter shows them phase-shifted. A single-acquire "both
  channels" command (if one exists) would fix it; otherwise the readout can't
  reproduce cross-channel timing.

### Reference rendering model (how to draw the trace like the scope)

A faithful trace is drawn from a **point list** `(x, y)` (x = sample index,
y = sample value) using **fixed scale factors** — never a per-frame auto-fit;
that fixed scale is what keeps the trace anchored like a real scope:

```
x_px = left_margin − ftol(x · HzRatio)                 ; HzRatio from time/div
y_px = pivot + ftol(y · VtRatio) + displacement        ; VtRatio from V/div
```

- **`VtRatio`** (vertical) and **`HzRatio`** (horizontal) are floats derived from
  the channel's V/div and the timebase; both are set once per acquisition and
  applied uniformly. `ftol` = truncate-to-int.
- **`pivot`** = the graticule centre (window dimension ÷ 2). The grid is **8×10
  divisions**; horizontal density is **200 points/division**.
- **`displacement`** = the channel's vertical placement — for analog this is
  `VERT-CHx-POS` (1/25-div units → divisions); for LA it is the per-channel row
  baseline.
- Consecutive mapped points are joined by **line segments** (Vectors mode;
  `DISPLAY-MODE`=1 draws Dots instead) with a per-channel colour.

**Special sample values (sentinels — must be handled, not plotted as data):**
- `y == 0xFFFF_F9F2` (−1550) → **pen-up / gap**: the trace breaks here (do not
  connect across it). Corresponds to the off-screen/rail-pinned samples in §5.
- `y == 0xFFFF_FB30` (−1232) → **trigger-column marker**, drawn at the trigger
  sample index.

**Analog:** one polyline per channel; `VtRatio` from V/div, `pivot` = centre,
`displacement` = `VERT-CHx-POS`.

**LA:** the *same* point→pixel mapping, but the 16 digital channels are each
drawn in their **own row** (per-channel `displacement`); every Dn is a 0/1
square wave scaled by `VtRatio` to a small row height, with the D0–D15 labels
and the enabled/selected-channel row highlight drawn alongside.

The practical fix for a naïve plotter: stop auto-scaling to the data's min/max;
instead scale vertically by a **fixed** counts-per-division, place each channel
by its `VERT-CHx-POS`, frame it on an 8×10 grid, and break the line at the gap
sentinel.

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
"vdiv" before; index→time/div table = `TB_TO_NS`, see Appendix E).

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

**Inferred / open:**
- Enum-coded field values (coupling, trigger modes, `[TRIG-STATE]` beyond
  {3 run, 4 scan, 6 re-arm}, …).
- Meaning of param `0x12` (vendor toggles it 1→0 per refresh; run/hold?).
- Host-side control (likely key-press events via `/keyprotocol.inf`, unconfirmed).
- counts→volts transfer scaling (does not track display V/div — §5) and
  calibration (may need a scope cal file).

---

## 8. Implementation notes for reuse

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
--- MEASURE (8 slots, each: SRC byte then type byte; up to 8 simultaneous) ---
188  1  MEASURE-ITEM1-SRC          source: 0=CH1 1=CH2 3=LA (id 2 skipped; no Math source)
189  1  MEASURE-ITEM1              type: 0=Off(empty slot) 1=Frequency 2=Period 3=Mean 4=Pk-Pk 5=Cyc RMS 6=Minimum 7=Maximum 8=Rise Time 9=Fall Time 10=+Width 11=-Width 12=Delay1-2 Rise 13=Delay1-2 Fall 14=+Duty 15=-Duty 16=Vbase 17=Vtop 18=Vmid 19=Vamp 20=Overshoot 21=Preshoot 22=Period Mean 23=Period RMS 24=FOvershoot 25=RPreshoot 26=Burst Width 27=FRF 28=FFR 29=LRR 30=LRF 31=LFR
190  1  MEASURE-ITEM2-SRC          source  (same enum as ITEM1-SRC)
191  1  MEASURE-ITEM2              type    (same enum as ITEM1)
192  1  MEASURE-ITEM3-SRC          source  (same enum)
193  1  MEASURE-ITEM3              type    (same enum)
194  1  MEASURE-ITEM4-SRC          source  (same enum)
195  1  MEASURE-ITEM4              type    (same enum)
196  1  MEASURE-ITEM5-SRC          source  (same enum)
197  1  MEASURE-ITEM5              type    (same enum)
198  1  MEASURE-ITEM6-SRC          source  (same enum)
199  1  MEASURE-ITEM6              type    (same enum)
200  1  MEASURE-ITEM7-SRC          source  (same enum)
201  1  MEASURE-ITEM7              type    (same enum)
202  1  MEASURE-ITEM8-SRC          source  (same enum)
203  1  MEASURE-ITEM8              type    (same enum)
--- CONTROL (menu/UI state) ---
204  1  CONTROL-TYPE               always 0 observed
205  1  CONTROL-MENUID             current menu id (see table below)
206  1  CONTROL-DISP-MENU          0/1 menu displayed on screen
--- LOGIC ANALYZER (menu 61 base; 62 = D7-D0 config page, 63 = D15-D8 page) ---
207  1  LA-SWI                     0/1 logic-analyzer on/off
208  2  LA-CHANNEL-STATE           D0..D15 enable bitmask, bit N = D(N) (D0=LSB; all-on=0xFFFF; low byte=D0-D7, high byte=D8-D15)
210  1  LA-CURRENT-CHANNEL         selected channel 0..15 (= D0..D15)
211  1  LA-D7-D0-THRESHOLD-TYPE    0=TTL 1=CMOS 2=ECL 3=User (threshold is per 8-ch group)
212  1  LA-D15-D8-THRESHOLD-TYPE   0=TTL 1=CMOS 2=ECL 3=User
213  2  LA-D7-D0-USER-THRESHOLD-VOLT   signed; volts = raw/4096 (±8V, 12-bit DAC = code<<4). Active when TYPE=User
215  2  LA-D15-D8-USER-THRESHOLD-VOLT  signed; same encoding (volts = raw/4096)
(217 = checksum)
```

### Enum tables (mapped so far)

| field | value → meaning |
|---|---|
| `TRIG-STATE` | 0 STOP · 1 WAIT (Normal, no trig) · 2 AUTO (no trig) · 3 TRIG'D · 4 SCAN/roll · 5 SINGLE (armed) · 6 ARMING flicker. Official on-screen labels: `STOP` / `Ready` / `AUTO` / `Trig'd` / `Scan` / `Astop` / `Armed` (0–6). |
| `TRIG-TYPE` | 0 Edge · 1 Video · 2 Pulse · 3 Slope · 4 Overtime · 5 Alter (swap; alternates `TRIG-SRC` CH1↔CH2) |
| `TRIG-SRC` | 0 CH1 · 1 CH2 · 2 EXT · 3 EXT/5 · 4 AC line. **Selectable set is restricted per trigger type:** Edge = all 5; Video / Pulse / Slope = CH1/CH2/EXT/EXT-5 (no AC line); Overtime = **CH1/CH2 only**. |
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
| `MEASURE-ITEMn-SRC` (n=1…8) | 0 CH1 · 1 CH2 · 3 LA (id 2 skipped/unused — **no Math source** in measurements) |
| `MEASURE-ITEMn` (n=1…8) | measurement type / `0` = **Off** (empty slot): 0 Off · 1 Frequency · 2 Period · 3 Mean · 4 Pk-Pk · 5 Cyc RMS · 6 Minimum · 7 Maximum · 8 Rise Time · 9 Fall Time · 10 +Width · 11 −Width · 12 Delay1-2 Rise · 13 Delay1-2 Fall · 14 +Duty · 15 −Duty · 16 Vbase · 17 Vtop · 18 Vmid · 19 Vamp · 20 Overshoot · 21 Preshoot · 22 Period Mean · 23 Period RMS · 24 FOvershoot · 25 RPreshoot · 26 Burst Width · 27 FRF · 28 FFR · 29 LRR · 30 LRF · 31 LFR |
| `LA-CHANNEL-STATE` | D0…D15 enable bitmask, **bit N = D(N)** (D0 = LSB, all-on = `0xFFFF`; D0–D7 = low byte, D8–D15 = high byte) |
| `LA-CURRENT-CHANNEL` | selected channel 0…15 (= D0…D15) |
| `LA-D7-D0-THRESHOLD-TYPE` / `LA-D15-D8-THRESHOLD-TYPE` | 0 TTL · 1 CMOS · 2 ECL · 3 User (threshold is **per 8-ch group**) |
| `LA-*-USER-THRESHOLD-VOLT` | signed; **volts = raw/4096** (±8 V, 12-bit DAC stored as `code<<4`); active when that group's TYPE = User |
| `VERT-CHx-VB` | V/div: 0=2mV 1=5mV 2=10 3=20 4=50 5=100 6=200 7=500mV 8=1V 9=2V 10=5V (10V/div → 0) |
| `HORIZ-*TB` | time/div index, 2-4-8 sequence 2 ns…40 s (`TB_TO_NS`); WIN-TB = knob 0..31, HORIZ-TB clamps at 6 (200 ns) |

**Measure** puts real state in the blob — 8 slots `MEASURE-ITEM1…8`, each a
(`-SRC`, type) pair — up to **8 simultaneous measurements** (confirmed on the
scope). Mapped 2026-07-09 by sweeping `MEASURE-ITEM8` through the on-screen list
(`captures/mso5202d-measure.pcapng`); enum above. Menu ids: **20** = base, **21**
= the item add/config submenu (the poll toggles `20↔21` as you set each item).

**Logic analyzer** is fully mapped (2026-07-09, `captures/mso5202d-la-*.pcapng`),
using the ESP32 16-channel test generator (`scripts/esp_toggler/`) as known
inputs: `LA-CHANNEL-STATE` bit N = D(N) (verified toggling each D0–D15
individually), `LA-CURRENT-CHANNEL` = selected 0–15, per-group threshold
type (TTL/CMOS/ECL/User) and user volts (raw/4096, ±8 V 12-bit DAC). Menu ids
61 base / 62 (D7-D0 page) / 63 (D15-D8 page).

Still-unmapped: the EXT-source trigger level in volts; FFT window codes 3/4
(Bartlett/Blackman, inferred).

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
| 10 | default / no active menu (vendor-app baseline); **also Utility page 3** (reused generic id) |
| 11 | Trigger menu (level/base, Edge default) |
| 15 | Cursor menu (cursor state is **not** in the settings blob) |
| 16 | Math → FFT submenu, **page 1** (source / window) |
| 17 | **Acquire** menu |
| 18 | **Save/Recall** → SETUP page |
| 19 | **Save/Recall** → REF (reference-waveform) page |
| 20 | **Measure** base menu |
| 21 | **Measure** → item add/config submenu (`MEASURE-ITEM*`) |
| 22 | Trigger → Slope submenu, **page 1** |
| 23 | Trigger → Slope submenu, **page 2** (V1/V2 / When / Time) |
| 24 | Trigger → Alter submenu (base) |
| 25 | **Default Setup** (factory reset — resets all settings-blob params to defaults) |
| 26 / 27 / 28 / 29 | Alter → **CH1** Edge / Pulse / Video / Overtime |
| 30 / 31 / 32 / 33 | Alter → **CH2** Edge / Pulse / Video / Overtime |
| 38 | Trigger → Overtime submenu, **page 1** |
| 39 | Trigger → Overtime submenu, **page 2** (Coupling) |
| 36 | Display menu (Grid / Format page) |
| 40 | Horizontal menu, **page 2** (holdoff / play-stop / coarse-fine) |
| 41 | Math menu (operations; `[MATH-DISP]` = math on/off) |
| 42 | **Utility** page 1 (system status / update fw / save wave / self-cal) |
| 43 | **Utility** page 2 |
| 47 | **Save/Recall** base (type selector) |
| 48 | **Save/Recall** → CSV **and** FileList (shared file-browser page) |
| 56 | Math → FFT submenu, **page 2** (zoom factor / vertical scale) |
| 61 | Logic Analyzer base menu (`[LA-SWI]` = LA on/off) |
| 62 | LA channel-config submenu — **D7-D0 group** (enables + threshold) |
| 63 | LA channel-config submenu — **D15-D8 group** |

Note some menus set `[CONTROL-MENUID]` but keep it constant while open (so it
shows in the value, not as a change) — e.g. CH1=1, CH2=2, Acquire=17 stayed put
throughout their captures; only `[CONTROL-DISP-MENU]` toggled 0↔1.

Multi-page trigger submenus use **consecutive ids** for page 1 / page 2
(Pulse 6/7, Slope 22/23, Overtime 38/39). `CONTROL-DISP-MENU` = 1 while a menu is shown, 0 when
closed. `CONTROL-TYPE` stayed 0 in every capture. (`MENU_NAMES` in
`mso5202d.py`.) Remaining menu ids (LA sub-pages, Measure statistics…) to be
mapped by opening each menu.

**Utility is view-only** — mapped 2026-07-09 by a page-cycle `CONTROL-MENUID`
poll (`captures/mso5202d-utility.pcapng`). Like Save/Recall it puts **no
parameters in the settings blob** (system
status, firmware update, save-waveform, self-cal are actions; `/protocol.inf`
has no Utility/sound/language/cal fields — only the front-panel `[MENU-UTILITY-KEY]`).
Its **3 pages cycle `42 → 43 → 10`** (menu visible throughout): page 1 = **42**
(system status / update fw / save wave / self-cal), page 2 = **43**, page 3 =
**10** — page 3 gets **no dedicated id, reusing the generic `10`** (same value as
the no-menu baseline, but here with `CONTROL-DISP-MENU`=1).

**Save/Recall (Storage) is view-only** — mapped 2026-07-09 by two ordered-open
`CONTROL-MENUID` polls (base → REF → SETUP → CSV → FileList). Like Cursor and the
Horizontal window/marks, it puts **no parameters in the settings blob**: through
both captures the only field changes were an incidental vertical-knob touch;
`/protocol.inf` has no REF/SETUP/CSV/FILE params (only `[ACQURIE-STORE-DEPTH]`).
So setups (1–10 slots), Ref-A/B waveform save, and CSV export are **actions, not
poll-able state**. Base menu = **47**; its sub-types are **19** (REF), **18**
(SETUP), **48** (CSV). **FileList reuses 48** — it is the CSV/USB file-browser
page, not a distinct type — so CSV and FileList are indistinguishable over the
poll.

---

## Appendix E — setting ranges, steps & units

Consolidated limits/divisions/units for every setting decoded so far. "Field" is
the `/protocol.inf` param (Appendix D gives its offset/enum); "raw" is the stored
value. Sequences and endpoints are hardware-verified unless marked *inferred*.
Units for the MSO5202D (200 MHz); the 60/100 MHz base models stop at 4 ns/div.

### Vertical (per channel)

| Setting | Field | Range | Step / sequence | Unit / notes |
|---|---|---|---|---|
| Volts/div | `VERT-CHx-VB` | 2 mV … 5 V/div | **1-2-5**, 11 steps (idx 0–10 = 2/5/10/20/50/100/200/500 mV, 1/2/5 V) | V/div; **×probe** scales the displayed value. 10 V/div reads back `VB=0` (quirk) |
| Vertical position | `VERT-CHx-POS` | **±8 div** (raw ±200) | 1 raw = **1/25 div** | signed; 25 counts/div. Knob-push → 0 (centre) |
| Probe ratio | `VERT-CHx-PROBE` | 1× / 10× / 100× / 1000× | 4 discrete | attenuation multiplier |
| Fine / coarse | `VERT-CHx-FINE` | Coarse / Fine | 2 | Fine = continuous V/div between the 1-2-5 steps |

### Horizontal / timebase

| Setting | Field | Range | Step / sequence | Unit / notes |
|---|---|---|---|---|
| Time/div (displayed) | `HORIZ-WIN-TB` | **2 ns … 40 s/div** | **2-4-8**, 32 steps (idx 0–31) | s/div (`TB_TO_NS`) |
| Time/div (acquisition) | `HORIZ-TB` | 2 ns … 200 ns, then **clamps** | same seq, clamps at idx 6 | s/div; ≥200 ns the acquisition TB stays at 200 ns while the display zooms |
| Horizontal delay | `HORIZ-TRIGTIME` | wide | 1 ps | **signed int64 ps**; negative = post-trigger. Knob-push → 0 |
| Sample rate | *derived* | ties to time/div | — | **200 Sa/div** → `Sa/s = 200 / (s/div)`; interval `= (s/div)/200` |
| Acquisition span | *derived* | — | — | 3840-sample block = **19.2 div** |

### Trigger

| Setting | Field | Range | Step | Unit / notes |
|---|---|---|---|---|
| Trigger level | `TRIG-VPOS` | **±8 div** (raw ±200) | 1/25 div | volts = `(VPOS − POS_src) × vdiv / 25` |
| Slope V1 / V2 | `TRIG-SLOPE-V1/V2` | ±8 div | 1/25 div | same volts calibration as level |
| Holdoff | `TRIG-HOLDTIME` | **100 ns … 10 s** | ps | int64 ps (`-MIN`=100 000, `-MAX`=10¹³) |
| Pulse / Slope / Overtime time | `TRIG-{PULSE,SLOPE,OVERTIME}-TIME` | **20 ns … 10 s** | ps | int64 ps |
| Video line # | `TRIG-VIDEO-LINE` | 1…525 (NTSC) / 1…625 (PAL) | 1 | line number |

### Acquire

| Setting | Field | Range | Step | Unit / notes |
|---|---|---|---|---|
| Average count | `ACQURIE-AVG-CNT` | 4 … 128 | **×2**, 6 steps (idx 0–5 = 4/8/16/32/64/128) | averages |
| Memory depth | `ACQURIE-STORE-DEPTH` | 4K / 40K / 512K / 1M | gapped codes (0/4/6/7) | samples; 1M = single-channel only |

### Display

| Setting | Field | Range | Step | Unit |
|---|---|---|---|---|
| Contrast | `DISPLAY-CONTRAST` | 0 … 15 | 1 | level (max = `-MAXCONTRAST`=15) |
| Grid brightness | `DISPLAY-GRID-BRIGHT` | 0 … 15 | 1 | level (max = `-MAXGRID-BRIGHT`=15) |
| Persistence | `DISPLAY-PERSIST` | Auto … Infinity | 9 discrete (0.2/0.4/0.8/1/2/4/8 s) | s |

### Math / FFT

| Setting | Field | Range | Step | Unit |
|---|---|---|---|---|
| FFT horizontal zoom | `MATH-FFT-FACTOR` | ×1 / ×2 / ×5 / ×10 | 4 discrete | zoom |
| FFT vertical scale | `MATH-FFT-DB` | 1 / 2 / 5 / 10 / 20 | 5 discrete | dB/div |

### Measure

| Setting | Field | Range | Step | Unit |
|---|---|---|---|---|
| Simultaneous items | `MEASURE-ITEM1…8` | up to **8** | — | slots |
| Measurement type | `MEASURE-ITEMn` | 0 … 31 (32 types) | 1 | enum (Appendix D) |
| Source | `MEASURE-ITEMn-SRC` | CH1 / CH2 / LA | — | 0/1/3 (2 unused) |

### Logic analyzer

| Setting | Field | Range | Step | Unit / notes |
|---|---|---|---|---|
| Channels | `LA-CHANNEL-STATE` | D0 … D15 | per-bit | bitmask, bit N = D(N) |
| Selected channel | `LA-CURRENT-CHANNEL` | 0 … 15 | 1 | = D0…D15 |
| User threshold (per group) | `LA-*-USER-THRESHOLD-VOLT` | **±8 V** | ≈**3.9 mV** (16 raw = `code<<4`, 12-bit DAC) | volts = raw/4096 |

---

## Appendix F — Host-side control (PC → scope) — CONFIRMED

The PC can fully drive the scope over the same bulk protocol. Decoded 2026-07-09
from USBPcap captures of the vendor **"Scope 2.0.0.6"** app (`captures/control/`,
scope-only), cross-referenced against our own field decode. **Three OUT command
paths**, all leader `0x53` with the standard framing + checksum (§3); each gets
an IN acknowledgement (`selector | 0x80`). This resolves the long-standing
**host-side control** question — how the PC drives the scope.

### F.1 Write settings — selector `0x11`

```
OUT  53 | D7 00 | 11 | <213-byte settings block> | ck        ; len 0x00D7 = 215
IN   53 | 03 00 | 91 | 00 | ck                               ; write ack (echo 0x91, status 0)
```

The 213 bytes are the **exact `/protocol.inf` parameter block** — identical
layout to the `0x01` poll *response* (Appendix A/D), just without the `0x81`
echo. So PC control of settings is **read-modify-write**: poll `0x01`, change the
field(s), write the whole block back with `0x11`.

Verified by decoding captured writes with `decode_settings()` (feed `0x81` + the
213 bytes): in `vertical_ch1_ch2` CH1 V/div stepped 5 V→500 mV→2 V→200 mV and
both `VERT-CHx-DISP` toggled; in `LA` the `LA-CHANNEL-STATE` mask walked D0…D3
off — matching our field semantics exactly. **This sets any settings-blob field**
(V/div, position, timebase, trigger, acquire, display, math, measure, LA, …).

### F.2 Front-panel key events — selector `0x13`

```
OUT  53 | 04 00 | 13 | <keyid> | <state> | ck                ; state 01 = press
IN   53 | .. | 93 | ...                                       ; key ack (echo 0x93)
```

`keyid` is the **0-indexed position in `/keyprotocol.inf`** (Appendix B). This
presses a physical key — the way to invoke **actions** that aren't settings
(autoset, run/stop, force, default-setup, single) and the ±/menu keys. The app's
on-screen "virtual panel" buttons all emit these. Verified in `systemfunctions`:
autoset→17, Default-Setup→21, Run/Stop→19, Trig-50%→46, Force→47.

Full key id map (index → `/keyprotocol.inf` name):

| id | key | id | key | id | key |
|---|---|---|---|---|---|
| 0–7 | `FN-0`…`FN-7` (softkeys F1–F8) | 24 | `VT-CH1-MENU` | 36 | `HZ-MENU` |
| 8 | `FN-MLEFT` (multi-knob ←) | 25 | `VT-CH1-PSUB` (pos −) | 37 | `HZ-PSUB` (delay −) |
| 9 | `FN-MRIGHT` (multi-knob →) | 26 | `VT-CH1-PADD` (pos +) | 38 | `HZ-PADD` (delay +) |
| 10 | `FN-MZERO` (multi-knob push) | 27 | `VT-CH1-PZERO` (pos 0) | 39 | `HZ-PZERO` (delay 0) |
| 11 | `MENU-SR` (Save/Recall) | 28 | `VT-CH1-VBSUB` (V/div −) | 40 | `HZ-TBSUB` (timebase −) |
| 12 | `MENU-MEASURE` | 29 | `VT-CH1-VBADD` (V/div +) | 41 | `HZ-TBADD` (timebase +) |
| 13 | `MENU-ACQUIRE` | 30 | `VT-CH2-MENU` | 42 | `TG-MENU` |
| 14 | `MENU-UTILITY` | 31 | `VT-CH2-PSUB` | 43 | `TG-PSUB` (level −) |
| 15 | `MENU-CURSOR` | 32 | `VT-CH2-PADD` | 44 | `TG-PADD` (level +) |
| 16 | `MENU-DISPLAY` | 33 | `VT-CH2-PZERO` | 45 | `TG-PZERO` (level 0) |
| 17 | `CT-AUTOSET` | 34 | `VT-CH2-VBSUB` | 46 | `TG-PHALF` (level 50%) |
| 18 | `CT-SINGLESEQ` | 35 | `VT-CH2-VBADD` | 47 | `TG-FORCE` (force trig) |
| 19 | `CT-RS` (Run/Stop) | 23 | `VT-MATH-MENU` | 48 | `TG-PROBECHECK` |
| 20 | `CT-HELP` | 21 | `CT-DS` (Default Setup) | 22 | `CT-STU` |

### F.3 Screen framebuffer (screenshot) — selector `0x20`

```
OUT  53 | 02 00 | 20 | ck                                    ; bare, no args
IN   53 | <len> | a0 | 01 | <RGB565 pixels…> | ck   (×N)      ; data chunks (~10 KB each)
IN   53 | 04 00 | a0 | 02 | <b> | ck                         ; end-marker
```

The scope streams its **raw LCD framebuffer** — echo `0xa0`, using the same
multi-frame subtype pattern as file-read/waveform (`00` size / `01` data / `02`
end). Pixels are **RGB565**, ≈**770 KB per screen** (consistent with 800×480).
The app polls `0x20` continuously to mirror the display — it is a genuine
**screenshot stream, not a local re-render** from parameters (`virtual_panel_play`:
9337 `0xa0` frames, ~93 MB of pixels; the uniform `0x3193` value is the graticule
background).

### IN acknowledgement echoes (all commands)

| OUT selector | IN echo | payload |
|---|---|---|
| `0x01` poll | `0x81` | 213-byte settings block |
| `0x02` acquire | `0x82` | waveform (subtyped) |
| `0x10` file read | `0x90` | file bytes (subtyped) |
| `0x11` **write settings** | `0x91` | status byte (00 = ok) |
| `0x12` param write | `0x92` | ack |
| `0x13` **key event** | `0x93` | ack |
| `0x20` **screen grab** | `0xa0` | RGB565 framebuffer (subtyped) |
