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
  in `MSO5202D-protocol.md`), recovered from USB captures of the vendor app — it
  is not present as strings in the vendor binaries.

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

The scope command bytes are built in the vendor app's compiled code and are **not**
present as strings in the binaries, so static analysis alone could not recover
them. They were obtained by capturing the vendor Windows app ("Scope 2.0.0.6")
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
1. **Resolve the settings-blob field alignment** (there is an unmodeled prefix; the
   naive `/protocol.inf` width-sum does not line up with observed offsets).
2. **Dump more scope files** via selector `0x10` (`/system.inf`, `/cal.inf`, …) to
   find the counts→volts / index→units **calibration table** — we can do this now
   directly with `scripts/mso5202d.py`.
3. **Crack 2-channel readout** (`0x12` does not switch channels).
4. **Host-side control:** find the command that presses a `/keyprotocol.inf` key
   (likely how the PC sets V/div, timebase, trigger, autoset, …).
