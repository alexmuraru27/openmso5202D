# Hantek MSO5202D — OpenHantek support investigation

**Goal:** add support for the Hantek MSO5202D to the OpenHantek fork
(`~/Projects/openhantekfork`), as originally requested in
[OpenHantek issue #302](https://github.com/OpenHantek/openhantek/issues/302).

**Status: WORKING Linux driver.** The protocol is reverse-engineered AND
`mso5202d_probe.py` drives the real scope end-to-end: connects, reads
`/protocol.inf` + `/keyprotocol.inf`, decodes live settings (timebase + V/div
verified against the front panel), and captures waveforms (1 kHz cal signal
confirmed as a clean square wave). See `MSO5202D-protocol.md` for the full spec
incl. the confirmed Linux connection recipe (detach cdc_subset → dev.reset() →
claim → clear_halt → IN-read-before-OUT-write).

Remaining open items: (1) real 2-channel readout — the `0x12` param does NOT
switch channels (ch0/ch1 return identical data); (2) host-side SET commands for
V/div/timebase (front-panel-only in the captures); (3) index→real-units cal
table (likely another `/*.inf`). None block basic single-channel capture.

**Unit under test:** MSO5202D, SW `3.2.35(180502.0)`, HW `1020x55778344`.
Investigated 2026-07-07.

---

## 1. Executive summary

- The MSO5202D is **not** a Cypress-FX2 USB scope adapter like the DSO-2xxx/52xx
  family OpenHantek was built for. It is a standalone benchtop scope running
  **embedded Linux 3.2.35** on a Samsung S3C ARM SoC (`s3c-hsudc` UDC).
- Over USB it presents as a **vendor-specific bulk device** (`049f:505a`,
  `bDeviceClass=255`) with two bulk endpoints. It emulates the **Cypress EZ-USB**
  device interface.
- On **Linux**, the generic `cdc_subset` driver grabs it as `usb0` purely because
  `049f:505a` is that driver's built-in default ID — this is misleading; the
  device is not really used as a network interface.
- On **Windows**, the vendor driver `dstusb.sys` (a Cypress **`ezusb.sys`**
  derivative) claims it as `\\.\Ezusb-0`, and the GUI app "Scope 2.0.0.6" drives
  it with standard EZ-USB IOCTLs (bulk read/write + vendor requests).
- **This transport model is exactly what OpenHantek already implements.** So the
  device *can* be integrated. What is missing is the concrete command/sample
  protocol, which the vendor app builds in compiled code and is not recoverable
  from strings.

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
- No FX2 firmware upload involved (the device runs its own Linux firmware).

### 2.3 Device family
This is a member of the well-known Hantek / Tekway / Voltcraft "DSO hack"
oscilloscope family (Samsung S3C + embedded Linux), extensively discussed on the
[EEVblog "Hantek/Tekway DSO hack" thread](https://www.eevblog.com/forum/testgear/hantek-tekway-dso-hack-get-200mhz-bw-for-free/).

---

## 3. Windows vendor software analysis (static RE)

Source: `~/Downloads/MSO5000D_Software/` (installer CD image).

### 3.1 The driver — `Driver/dstusb.{inf,sys,cat}`
- `dstusb.inf` binds `USB\VID_049f&PID_505a`, device description "Measurement
  Device", custom `ClassGuid = {5444534f-1100-2008-0218-080111008219}`
  (`5444534f` = ASCII "TDSO").
- Service binaries: `dstusbx86.sys` / `dstusbamd64.sys` / `dstusbia64.sys`.
- The driver (UTF-16 strings) creates:
  ```
  \Device\Ezusb-0
  \DosDevices\Ezusb-0
  "DSO usbdriver", "usbdriver", "usbad.sys"
  ```
  → It is a **Cypress EZ-USB driver derivative** (`ezusb.sys`). The scope
  emulates the EZ-USB device interface. Its symbolic link is `\\.\Ezusb-0`.

### 3.2 The application — "Scope 2.0.0.6"
- `Setup.exe` is an **8 MB Wise installer** (not a plain archive; 7z/cabextract
  can't open it). Extracted by carving its deflate streams — see
  `carve.py` in this folder (37 streams, ~20 valid PE files).
- The main GUI app carves out as an **MFC C++ application**, version string
  **"Scope 2.0.0.6"**, title "DIGITAL STORAGE OSCILLOSCOPE". UI strings include
  "Connect to oscilloscope", "VOLTS/DIV", "TRIGGER", classes
  `CWaveformMeasurementView`, `CWaveformTabularDoc`, etc.
- Imports of interest: **only `CreateFileA` + `DeviceIoControl`** (no SetupAPI).
  It references the literal string **`Ezusb-0`** → it opens `\\.\Ezusb-0` and
  issues **EZ-USB IOCTLs** (BULK_READ/BULK_WRITE/VENDOR_REQUEST) to talk to the
  scope. No TCP/network sockets involved.

### 3.3 What static RE could NOT recover
The actual **scope command bytes** (what the app writes to bulk OUT / which
vendor requests it issues, and the sample-data framing on bulk IN) are built in
compiled MFC code and passed through `DeviceIoControl`. They are **not** present
as strings. Recovering them needs either a live USB capture or full disassembly
(Ghidra/IDA) of the app.

---

## 4. Why this fits OpenHantek (transport compatibility)

OpenHantek's architecture is: send command frames on a bulk **OUT** endpoint,
read sample data on a bulk **IN** endpoint, and issue **vendor control requests**
for setup (gain/offset/relays). That is precisely the EZ-USB model the MSO5202D
uses. Concretely, in the OpenHantek code (`~/Projects/openhantekfork`):

| Aspect | OpenHantek expects | MSO5202D | Fit |
|---|---|---|---|
| Interface | vendor-spec class, 2 endpoints (`usbdevice.cpp:81-83`) | vendor-spec, 2 endpoints | ✅ matches |
| Bulk OUT | `HANTEK_EP_OUT = 0x02` (`usbdevicedefinitions.h`) | `0x02` | ✅ matches |
| Bulk IN | `HANTEK_EP_IN = 0x86` (hardcoded) | `0x81` | ❌ needs per-model IN endpoint |
| Kernel driver | (none on the FX2 scopes) | `cdc_subset` holds the interface | ❌ must detach before claim |
| Command bytes | Hantek DSO codes (`Bulk*`/`Control*`) | unknown, maybe similar | ❓ needs capture |
| Firmware upload | FX2 `.hex` upload | none (embedded Linux) | ✅ handled: no-firmware IDs |

**The two transport gaps (IN endpoint, kernel detach) are small, well-understood
code changes.** The one real unknown is the command/sample protocol.

---

## 5. What has been changed in the OpenHantek fork

All committed to the working tree; **builds cleanly** (`make OpenHantek`).
Model registration is **disabled** (`#if 0`) so OpenHantek does not advertise
support it cannot yet deliver.

- `openhantek/src/hantekdso/models/modelMSO5202.h` — model class + full findings
  in the header comment.
- `openhantek/src/hantekdso/models/modelMSO5202.cpp` — real IDs (`049f:505a`),
  no-firmware IDs set equal (so no FX2 upload is attempted), DSO-5200 command
  set + spec as **placeholders**, registration behind `#if 0`.
- `firmware/60-hantek.rules` — udev entry granting access to `049f:505a`.

These are an accurate record and a wiring point — **not** working support.

---

## 6. Next steps

### 6.1 Get the command protocol (the blocker) — USB capture on Windows
See `USB-capture-guide.md` in this folder. In short: capture the vendor app
talking to the scope with Wireshark + USBPcap, doing a scripted set of known
actions (connect, change V/div, change timebase, single capture). Bring the
`.pcapng` back for decoding of the bulk-OUT command frames and bulk-IN data.

### 6.2 Then, on the OpenHantek side
1. Add a per-model bulk **IN endpoint** (`0x81`) instead of the hardcoded `0x86`.
2. Detach the `cdc_subset` kernel driver before `libusb_claim_interface`
   (`libusb_set_auto_detach_kernel_driver(handle, 1)` in the connect path), or
   prevent `cdc_subset` from binding via a udev/modprobe rule.
3. Replace the placeholder `Bulk*`/`Control*` command codes and the sample-data
   decoding with the captured protocol; recalibrate gain / voltageLimit /
   samplerate from real data.
4. Enable registration (`#if 1`) and test against the hardware.

### 6.3 Optional shortcut worth trying first
Because the transport is EZ-USB-style and OpenHantek's DSO-5200 already targets
EZ-USB-style scopes, it is worth an experiment: apply gaps (1)+(2), point the
model at the DSO-5200 command set, enable it, and see whether the scope responds
at all. If any of the existing commands elicit sane data, it narrows the capture
work. (Low probability, but cheap to try once the endpoint + detach are done.)

---

## 7. Reproducibility artifacts (this folder)
- `MSO5202D-investigation.md` — this document (hardware + software findings).
- `MSO5202D-protocol.md` — the reverse-engineered wire protocol from the captures.
- `mso5202d.py` — **reusable driver library** (transport + protocol). `python3
  mso5202d.py` runs a self-test.
- `mso5202d_plot.py` — **live waveform viewer** built on the driver:
    - `python3 mso5202d_plot.py`            → live GUI window (run as your user;
      needs the udev rule so no sudo, which would break the GUI's X access)
    - `python3 mso5202d_plot.py --png o.png` → headless: save a PNG (for testing)
- `mso5202d_probe.py` — original standalone PoC/diagnostic (pyusb).
- `70-mso5202d.rules` — udev rule granting user access to 049f:505a.
- `carve.py` — extracts the deflate streams from the Wise `Setup.exe`.
- `USB-capture-guide.md` — how to capture the Windows protocol usefully.
- `mso5202d-session1.pcapng`, `mso5202d-session2.pcapng` — the USB captures.

## 8. Recommended next actions
1. **Run the PoC** (`pip install pyusb; sudo python3 mso5202d_probe.py`) to
   confirm we can drive the real device: read the `.inf` files, decode V/div +
   timebase, and pull a waveform. This proves end-to-end control before any
   OpenHantek work.
2. **Dump more scope files** via the read-file command (selector 0x10): try
   `/system.inf`, `/cal.inf`, etc., to find the index→units calibration table.
3. **Capture in-app control** (if the vendor app can set V/div etc. from the PC)
   to learn the host-side SET encodings; otherwise derive them from the
   settings-blob offset map + `/protocol.inf` order.
4. Decide integration shape: a standalone tool/plotter is likely a better fit
   than forcing this protocol into OpenHantek's DSO-specific model classes.
