# openmso5202D

An open driver + desktop GUI for the **Hantek MSO5202D** oscilloscope (USB `049f:505a`),
reverse-engineered from USB captures of the vendor Windows app.

> [!IMPORTANT]
> **A USB flash drive must be plugged into the scope's front-panel USB port.** Every capture
> is routed through the scope's own Save-to-CSV export, which writes to that drive and is read
> back over USB. With no drive mounted the save is a silent no-op and the app gets no data.

![The app decoding an SPI ramp captured at 40K memory depth](docs/screenshots/openmso5202D_spi.png)

The app is a **triggered capture-and-decode workbench**, not a live scope face. You set up the
acquisition on the left, take one deep, trigger-aligned record with the two buttons at the
bottom, and then read it: pan and zoom the traces, drop measurement cursors, and — if the
signal is UART, SPI or I²C — see every decoded byte both as a pill under the waveform and as a
list on the right. Records go up to **1M samples**, far past the 3,840 samples the scope will
serve over USB for a screen refresh.

- **`backend/`** — `mso5202d`, a reusable Rust driver crate. The lowest layer is the USB
  transport (`usb::Transport`): connect / reconnect / reset, interface binding, and the
  reader-thread-before-write transaction dance the scope requires. `Scope` is a thin
  high-level facade on top.
- **`frontend/`** — a [Tauri 2](https://tauri.app) desktop app. It depends on the
  `mso5202d` crate directly, so **launching the app launches the backend**: Tauri opens
  the scope over USB at startup and exposes it to the webview through commands.
- **`docs/`**, **`scripts/`**, **`scope_dump/`** — the reverse-engineering record: the
  wire-protocol spec, the Python reference tooling the Rust port is based on, and the raw
  captures.

The repo is one **Cargo workspace** (`backend` + `frontend/src-tauri`) and one **pnpm
workspace** (`frontend`).

## Prerequisites

- **Rust** (stable) with `cargo`
- **Node** + **pnpm**
- **libusb 1.0** (`libusb-1.0-0-dev`)
- **Tauri Linux deps**: `webkit2gtk-4.1`, `libgtk-3-dev`, plus the usual
  `build-essential`/`libssl-dev` (see the [Tauri prerequisites](https://tauri.app/start/prerequisites/))
- **USB access**: install the udev rule so the scope is reachable without root:
  ```sh
  sudo cp 70-mso5202d.rules /etc/udev/rules.d/
  sudo udevadm control --reload-rules && sudo udevadm trigger
  # then replug the scope
  ```
  Otherwise run the app/playground as root.

## Build & run

The project has **two stages**, driven from the repo root:

### 1. Bootstrap (once, and after dependency changes)

```sh
pnpm bootstrap
```

Installs the JS dependencies (`pnpm install`) and pre-fetches the Rust crates
(`cargo fetch`).

### 2. Build the standalone app

```sh
pnpm build
```

This is the release build — it type-checks and bundles the frontend, compiles the Rust backend
optimized, and packages everything into a **self-contained desktop app**. No `pnpm`, Node or
Vite is involved once it is built; the web assets are embedded in the binary.

It produces, under `target/release/`:

| Output | Path |
| --- | --- |
| Plain executable | `target/release/openmso5202d` |
| Debian package | `target/release/bundle/deb/openmso5202D_0.1.0_amd64.deb` |
| RPM package | `target/release/bundle/rpm/openmso5202D-0.1.0-1.x86_64.rpm` |
| AppImage (portable) | `target/release/bundle/appimage/openmso5202D_0.1.0_amd64.AppImage` |

Pick whichever suits you:

```sh
./target/release/openmso5202d                        # run it straight from the build
sudo apt install ./target/release/bundle/deb/*.deb   # install system-wide → app menu entry
chmod +x target/release/bundle/appimage/*.AppImage   # portable, copy it anywhere
```

Installing the `.deb`/`.rpm` registers `openmso5202D` as a normal desktop application, so it
appears in the launcher and can be started like any other program. The AppImage needs no
install at all — it is a single file you can move to another machine of the same
architecture.

Building only the executable (skipping the installer bundles) is faster:

```sh
pnpm --filter openmso5202d-frontend tauri build --no-bundle
```

Whichever way you run it, the udev rule from the prerequisites still applies — without it the
app cannot open the scope unless started as root.

### Develop

```sh
pnpm dev          # hot-reloading Tauri dev app (Vite dev server + the Rust backend)
```

## Using the app

The app opens the scope over USB at startup; the badge under the title shows the bus and
address it found. If the scope was plugged in late, or the udev rule was not yet in place,
press **Connect** to retry.

### 1. Set up the acquisition

The left sidebar is the whole setup, in four foldable sections. Each one shows a summary of
its current state while collapsed, so nothing is hidden.

- **Acquisition** — which channels to record (CH1, CH2 or both), the **max frequency** of the
  signal you expect and how many **samples per clock** you want of it, and the **memory
  depth** (4K / 40K / 512K / 1M).

  Frequency and samples-per-clock are how you pick a timebase: the app works out the SEC/DIV
  the scope needs, snaps it to the instrument's fixed ladder, and tells you what you actually
  get — *"Captures 400 µs at 20 µs/div — 100 samples/clock actual"*. If the ADC cannot sample
  that fast the hint says so. Deeper memory at the same timebase means the same time window
  with more samples; to record a *longer* stretch, lower the max frequency.

  1M is single-channel only, and picking a second channel drops it to 512K automatically.
- **Channel setup** — probe attenuation (1× / 10× / 100× / 1000×), coupling (DC / AC / GND),
  bandwidth (Full / 20 MHz) and invert, per channel. Probe attenuation matters: it scales both
  the volts on screen and the trigger level.
- **Trigger** — trigger type (Edge, Pulse, Video, Slope, Overtime, Alter), source, slope,
  coupling, mode and level. The level is entered in volts.
- **Protocol decode** — **None**, **UART**, **SPI** or **I²C**, plus which physical channel
  carries each line. UART needs one channel (data); SPI needs clock + data, I²C needs SCL +
  SDA, so both channels must be on. Assigning a line to the channel the other line is on swaps
  them.

  Decode settings are *not* part of the acquisition — changing the protocol or a line
  assignment re-decodes the record already on screen instantly, with no new capture.

Every setting is remembered between runs. **Reset settings** in the top bar puts them all
back to their defaults.

### 2. Capture

Two buttons, in order:

1. **① Prepare** — the slow one. It resets the scope to a known state, applies the whole
   sidebar configuration to it, and probes the signal to settle the timebase. A few seconds.
   Changing any acquisition setting (channel, depth, timebase, trigger, probe…) invalidates
   it and you must prepare again — the button for step ② greys out to say so.
2. **② Arm capture** — the fast one, re-pressable. It arms a single-sequence capture, waits
   for the trigger, and reads the record back. Each press gives a fresh record with the same
   setup.

The top bar shows a labelled progress bar for each phase, and any error stays there until
dismissed.

### 3. Read the record

The plot shows one lane per channel, with its V/div, pk-pk and probe factor in the corner.

- **Scroll wheel** — pan through the record sideways.
- **Left-drag** — pan; **right-drag** — zoom (sideways scales time, up/down scales the
  voltage axis of the lane under the pointer).
- **Click** — drop a measurement cursor on the trace. Cursors read out their time and voltage,
  plus the Δt, ΔV and implied frequency between them. Drag a cursor's dot to move it, and use
  the **×** button to clear them all.
- **⤢** — fit the whole record back into view.

With a protocol selected, each decoded byte is drawn as a pill over the stretch of waveform it
was read from, with faint byte-boundary guides. The **Decoded** list on the right gives every
byte as hex, decimal, character and timestamp; clicking a row zooms the plot to that byte and
brackets it with cursors, and moving a cursor highlights whichever byte it lands on.

### Files

![The waveform file library, listing the scope's card and local CSVs](docs/screenshots/openmso5202D_file_downloader.png)

**Files…** in the top bar opens the waveform library. Captures are exported by the scope as
`WaveData*.csv` onto the front-panel USB drive, and this dialog is how you manage them:

- **Scope card** — list what is on the drive, **Download** the ticked files to this computer,
  or **Clear all** to delete every `WaveData` CSV (irreversible; it uses the scope's own
  delete softkey, never a shell `rm`).
- **This computer** — **Add files…** brings any saved CSVs into the library.

Assign a file to **1** or **2** to load it as that channel and press **Plot traces** to view
and decode it. This works with **no scope connected**, so an old capture can be re-read and
re-decoded offline.

### Requirements & caveats

- **Every** capture, 4K included, routes through the scope's front-panel **USB flash drive** —
  it must be plugged in and mounted, or the save is a silent no-op and no record comes back.
- The 16-channel **logic analyser pod cannot be read live over USB** (the firmware path is
  broken and corrupts the scope's own display). LA data is only available through a saved CSV,
  and enabling the pod clamps memory depth to 4K.
- A deep save is slow: a 512K record is a ~7.7 MB file and takes the scope tens of seconds to
  write before it can be read back.