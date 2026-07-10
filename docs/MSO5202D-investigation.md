# Hantek MSO5202D — hardware & reverse-engineering investigation

This is the **hardware / RE-journey companion** to `MSO5202D-protocol.md` (which
is the authoritative wire-protocol reference). It records how the device was
identified and how the protocol was discovered.

**Status: WORKING Linux driver.** The protocol is reverse-engineered and the
driver (`scripts/mso5202d.py`) drives the real scope end-to-end: connects, reads
`/protocol.inf` + `/keyprotocol.inf`, decodes live settings, and captures
waveforms (1 kHz cal signal confirmed as a clean square wave). See
`MSO5202D-protocol.md` for the full spec, the Linux connection recipe and the
reasons behind each step, plus the current open items.

**Unit under test:** MSO5202D, SW `3.2.35(180502.0)`, HW `1020x55778344`.
Investigated 2026-07-07.

---

## 1. Executive summary

- The MSO5202D is a standalone benchtop scope running **embedded Linux 3.2.35** on
  a Samsung S3C ARM SoC (`s3c-hsudc` UDC) — not a simple USB "scope adapter".
- Over USB it presents as a **vendor-specific bulk device** (`049f:505a`,
  `bDeviceClass=255`) with two bulk endpoints. It emulates the **Cypress EZ-USB**
  device interface.
- On **Linux**, the generic `cdc_subset` driver grabs it as `usb0` purely because
  `049f:505a` is that driver's built-in default ID — this is misleading; the
  device is not really used as a network interface.
- On **Windows**, the vendor driver `dstusb.sys` (a Cypress **`ezusb.sys`**
  derivative) claims it as `\\.\Ezusb-0`, and the GUI app "Scope 2.0.0.6" drives
  it with standard EZ-USB IOCTLs (bulk read/write + vendor requests).
- The command/sample protocol is a custom `'S'`-framed protocol (fully documented
  in `MSO5202D-protocol.md`), recovered from USB captures of the vendor app.

---

## 2. Hardware / USB facts (from the real unit)

### 2.1 Enumeration (`dmesg`)
```
usb 1-7.3: New USB device found, idVendor=049f, idProduct=505a, bcdDevice=24.30
usb 1-7.3: Product: Gadget Serial v2.4
usb 1-7.3: Manufacturer: Linux 3.2.35 with s3c-hsudc
cdc_subset 1-7.3:1.0 usb0: register 'cdc_subset' at usb-...-7.3, Linux Device
```
So: embedded Linux gadget, Samsung S3C high-speed UDC. Linux's `cdc_subset`
usbnet driver binds it (creates `usb0`).

### 2.2 USB descriptor (`lsusb -v -d 049f:505a`)
```
bDeviceClass          255 Vendor Specific Class
bNumConfigurations      1
  bNumInterfaces        1
    bInterfaceNumber    0
    bInterfaceClass     255 Vendor Specific Class
    bNumEndpoints       2
      bEndpointAddress  0x81  EP 1 IN   Bulk  512 bytes
      bEndpointAddress  0x02  EP 2 OUT  Bulk  512 bytes
```
**Key facts:**
- Vendor-specific class → not really CDC; the transport is raw USB bulk.
- Bulk **IN = `0x81`**, bulk **OUT = `0x02`**, 512-byte packets (high-speed).
- Single interface, no alternate settings.
- No firmware upload involved (the device runs its own embedded Linux firmware).

### 2.3 Device family
This is a member of the well-known Hantek / Tekway / Voltcraft "DSO hack"
oscilloscope family (Samsung S3C + embedded Linux), extensively discussed on the
[EEVblog "Hantek/Tekway DSO hack" thread](https://www.eevblog.com/forum/testgear/hantek-tekway-dso-hack-get-200mhz-bw-for-free/).

---

## 3. How the protocol was recovered

The protocol was obtained by capturing the vendor Windows app ("Scope 2.0.0.6")
talking to the scope over USB (Wireshark/USBPcap), then decoding the bulk-OUT
command frames and bulk-IN data frames. The two captures live in `../captures/`
(stripped to MSO-only traffic); the vendor software is archived at
`docs/drivers/MSO5000D_Software.zip`. The resulting wire protocol — framing,
checksum, the file-read / poll / acquire handshakes, and the 8-bit sample format —
is documented in full in `MSO5202D-protocol.md`.

---

## 4. Repository layout

```
openmso5202D/
├── scripts/
│   ├── mso5202d.py         # driver library (transport + protocol); self-tests when run
│   ├── mso5202d_plot.py    # live waveform viewer (matplotlib/Tk; --png for headless)
│   └── mso5202d_probe.py   # original PoC / diagnostic
├── docs/
│   ├── MSO5202D-protocol.md        # authoritative wire-protocol reference (read this first)
│   ├── MSO5202D-investigation.md   # this document
│   └── drivers/MSO5000D_Software.zip  # vendor Windows software
├── captures/               # mso5202d-session1/2.pcapng (stripped to MSO-only 049f:505a)
├── 70-mso5202d.rules       # udev rule → /etc/udev/rules.d/ (run without sudo)
└── README.md, LICENSE
```

Running:
- `python3 scripts/mso5202d.py` — driver self-test (reads .inf, decodes settings, one waveform).
- `python3 scripts/mso5202d_plot.py` — live viewer (needs the udev rule; don't use sudo — it breaks the GUI's X access).

---

## 5. Recommended next actions

See `MSO5202D-protocol.md` §8 for the detailed reverse-engineering to-do. In brief:
1. ~~**Resolve the settings-blob field alignment**~~ — **DONE 2026-07-08** via a
   CH1 V/div knob-sweep capture (`captures/mso5202d-ch1-vdiv.pcapng`): the params
   start right after the `0x81` echo (raw offset 4), no prefix; the supposed
   "subtype 0x01" was `[VERT-CH1-DISP]=1`. See protocol doc §6. A time/div sweep
   (`captures/mso5202d-timediv.pcapng`) then mapped `[HORIZ-TB]`/`[HORIZ-WIN-TB]`
   → 2 ns…40 s (2-4-8 sequence, 32 steps; acquisition TB clamps at 200 ns), and a
   combined all-knobs capture (`captures/mso5202d-combined.pcapng`) resolved
   vertical-position/trigger-level units and the picosecond time fields. A
   trigger-level sweep (`captures/mso5202d-trig-level.pcapng`) then calibrated
   those units against the scope's V readout: **1/25 div** (`level_V =
   (TRIG-VPOS − POS_src) × vdiv/25`; ±200 = ±8 div), and pinned `TRIG-STATE`
   2 = untriggered. Same method still pending for the coupling/type/mode enums.
2. **Dump more scope files** via selector `0x10` (`/system.inf`, `/cal.inf`, …) to
   find the counts→volts / index→units **calibration table** — we can do this now
   directly with `scripts/mso5202d.py`.
3. ~~**Crack 2-channel readout**~~ — **DONE 2026-07-08**: the acquire value byte
   selects the channel (`02 01 00` = CH1, `02 01 01` = CH2); `0x12` is
   something else (run/hold?). See protocol doc §5. Off-screen positioning clips
   the waveform to the rails / returns rail-to-rail blocks
   (`captures/mso5202d-ch1-vpos.pcapng`). The vertical amplitude / counts→volts
   scale is still **unmodelled** — deterministic but differs per channel/V/div
   (CH1@5V/div→27 counts vs CH2@2V/div→192 counts for the same cal signal;
   fine/probe/trigger ruled out).
4. ~~**Sample rate / X-axis calibration**~~ — **DONE 2026-07-08**: 200
   samples/div (sample interval = time/div ÷ 200), block = 19.2 div. Confirmed
   against the vendor MSO5000-series manual and our own cal-signal cycle counts.
5. **Host-side control:** find the command that presses a `/keyprotocol.inf` key
   (likely how the PC sets V/div, timebase, trigger, autoset, …).

**Cross-checked against the vendor MSO5000-series user manual (Ch. 8 specs).**
Confirmed: 2-4-8 SEC/DIV sequence; holdoff 100 ns–10 s (⇒ ps time fields);
trigger level ±8 div (= VPOS ±200 ⇒ 1/25-div units); scan/roll mode at ≥80 ms/div
(⇒ `[TRIG-STATE]`=4); 200 samples/div; 8-bit ADC, channels sampled
simultaneously (⇒ non-interleaved dual readout). The manual is the 60/100 MHz
**MSO5000B** base model (SEC/DIV from 4 ns, VOLTS to 5 V/div); our 200 MHz
**MSO5202D** extends the fast end to 2 ns/div (TB index 0).

---

## 6. Menu capture — DONE

All front-panel menus are mapped (Save/Recall, Utility, Measure, LA — plus the
Vertical/Horizontal/Trigger/Acquire/Display/Math/Cursor menus done earlier). See
`protocol.md` Appendix D (`CONTROL-MENUID` table + enum tables) for the results.

Method (now checked in): `scripts/mso5202d_capture.py <sec> <out.pcapng>` for a
scope-only pcap, run alongside a settings poller that decodes each blob and logs
the changed `[…]` fields + `[CONTROL-MENUID]`; step the menu on the front panel.
For the **LA** capture an ESP32 (`scripts/esp_combo_gen/`) drove all 16
channels with per-channel distinct frequencies as known inputs. Every new
field/enum is folded into `protocol.md` (Appendix D) and the enum maps in
`mso5202d.py`.

Smaller leftovers still open: EXT/EXT-5 trigger level in volts; `MATH-FFT-WIN`
codes 3/4 (Bartlett/Blackman, inferred); the Display **refresh-rate** control
and the second FFT/wave-intensity 0–15 control (neither appeared in the blob).
