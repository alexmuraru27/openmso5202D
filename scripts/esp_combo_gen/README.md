# esp_combo_gen — combined analog + LA test generator

Drives an analog serial protocol on **CH1/CH2** *and* all **16 logic-analyzer
channels** at the same time, so a single MSO5202D acquisition can exercise the
analog serial decoders **and** the logic pod together.

## Wiring

| Signal | ESP32 | Scope |
|--------|-------|-------|
| CH1 (analog) | **GPIO22** | CH1 probe |
| CH2 (analog) | **GPIO23** | CH2 probe |
| LA D0…D15 | 13,12,14,27,26,25,33,32,15,2,4,16,17,5,18,19 | LA pod D0…D15 |
| GND | any GND | probe + pod ground |

The analog pins (22/23) are deliberately **separate** from the 16 LA pins, so all
16 logic channels stay independent (unlike the single-protocol sketches, which
put the serial line on GPIO13/14 = LA D0/D2). Outputs are **3.3 V CMOS**; set the
scope LA threshold ~**1.5 V**, and CH1/CH2 to **1× probe, DC, invert off**.

## What it generates

- **CH1/CH2 — a serial protocol** (selectable), looping the `0x00..0xFF` ramp.
  Protocol **and frequency are switchable at runtime** over the serial console
  (no reflashing). Each protocol has a **discrete table of frequencies**;
  `freq <hz>` snaps to the nearest entry:
  - **SPI**: SCLK=GPIO22, MOSI=GPIO23, mode 0, MSB — HW peripheral
    `1k 10k 50k 100k 250k 500k 1M 2M 4M 5M 8M 10M 12M 16M 20M` Hz
  - **UART**: TX=GPIO22, 8N1 (CH2 unused) — HW peripheral
    `300 1200 2400 4800 9600 14400 19200 38400 57600 115200 230400 460800 921600 1M 1.5M 2M 3M 5M` baud
  - **I²C**: SCL=GPIO22, SDA=GPIO23, self-ACK — bit-banged
    `1k 10k 50k 100k 400k 1M 3.4M 5M` Hz — but the bit-bang only *reaches*
    ~500 kHz, so the 1M/3.4M/5M entries report `freq_achieved` ≈ 500 kHz.
- **LA D0…D15 — a distinct frequency per line**, `f_N = 1000/(N+1)` Hz
  (D0 = 1 kHz … D15 = 62.5 Hz) — a known, per-channel-identifiable LA pattern.

The LA runs continuously (non-blocking `micros()` scheduler); the serial is paced
between LA ticks, and SPI/UART use the hardware peripherals so their bit timing
stays exact regardless of the LA.

## Runtime control (serial command API)

Send one command per line at 115200 8N1 (LF or CR). **Every command replies with a
single JSON line**, so it is easy to script; boot-banner lines are plain text.

| Command | Effect |
|---------|--------|
| `help` / `?` | usage string (JSON `help` field) |
| `id` / `ping` | `{"ok":true,"dev":"esp_combo_gen","api":1}` |
| `status` | full state: active protocol, frequency, achieved freq, per-protocol tables & last freqs |
| `range` / `list` | active protocol's `min`/`max` + full frequency `table` |
| `proto <spi\|uart\|i2c>` | switch protocol (restores that protocol's last-used freq) → replies `status` |
| `freq <hz>` | set frequency for the active protocol (**snapped to nearest table entry**) → replies `status` |
| `set <proto> <hz>` | switch protocol **and** set its frequency in one call → replies `status` |
| `mode <single\|continuous>` | preset the transmit pattern (see below) → replies `status` |
| `burst <1..256>` | bytes sent per transaction → replies `status` |
| `gap <us\|auto>` | idle microseconds between transactions (`0` = continuous) → replies `status` |

**Continuous vs framed.** Each transmit "unit" sends `burst` ramp bytes in **one
transaction** (SPI: a single `beginTransaction`/`endTransaction`; UART: one write
with no per-byte flush; I²C: one `START…STOP`), then idles for `gap` µs. So:
- `mode single` (default) = `burst 1`, auto gap (~30 bit-times) — **framed** bytes
  with idle gaps the serial decoders use to reframe. I²C's framed default is 4 B.
- `mode continuous` = `burst 64`, `gap 0` — a **solid, near-gapless stream** for
  looking at the waveform. Verified on hardware: at 500 kHz SPI this lifts the
  record's edge activity from ~3% (framed) to ~39% (continuous).

Tune the two independently with `burst`/`gap` (e.g. `burst 256 gap 0` for the
longest continuous SPI run, or `gap 500` to widen the idle between framed bytes).

`freq` means **SPI SCLK / UART baud / I²C SCL** and snaps to the nearest table
entry (so `freq 950000` on SPI selects 1 MHz). For high-mode I²C the bit-bang
can't reach the requested rate, so the reply reports both `freq` (requested) and
`freq_achieved` (what the pin actually toggles at).

### Host tool — `scripts/mso5202d_espgen.py`

```bash
cd scripts
python3 mso5202d_espgen.py status                      # active protocol + frequency + mode (JSON)
python3 mso5202d_espgen.py reset                       # reboot the ESP to its power-on defaults
python3 mso5202d_espgen.py capabilities                # every protocol + freq table + modes (JSON)
python3 mso5202d_espgen.py set spi 2000000 continuous  # protocol + frequency + transmit mode
python3 mso5202d_espgen.py set uart 115200 single      # mode: single=framed / continuous=stream
python3 mso5202d_espgen.py burst 256                   # bytes per transaction (fine-tune)
python3 mso5202d_espgen.py gap 0                       # idle us between transactions (0=continuous)
python3 mso5202d_espgen.py --json status               # raw JSON (for scripting)
```

`capabilities` prints every protocol with its full frequency table **and** the
transmit modes as JSON. `set <proto> <hz> <single|continuous>` is the single
interface to configure the generator; `burst`/`gap` fine-tune the mode. `reset`
reboots the board to its power-on defaults (same as `--reset`).

Auto-detects the port (`/dev/ttyUSB*`, `/dev/ttyACM*`; override with `--port`).
The connection is **non-disturbing** — it opens the tty with HUPCL cleared and
DTR/RTS held low, so it does **not** reset the ESP32; whatever you set keeps
running after the tool exits, and a `status` query never wipes it. Pass `--reset`
to force a reboot back to the power-on defaults. Pure `termios`/`ioctl`, no
pyserial. Don't run this while `pio device monitor` (or another program) holds
the port.

## Build / flash

```bash
cd scripts/esp_combo_gen
~/.pio-venv/bin/pio run -t upload            # build + flash (SPI is the power-on default)
~/.pio-venv/bin/pio device monitor           # 115200 baud — prints the pin map + JSON status
```

Protocol/frequency no longer need a reflash — set them at runtime with
`scripts/mso5202d_espgen.py` (see below). `-DPROTO=0/1/2` only picks the power-on
default protocol.

**PlatformIO setup note (Debian/Ubuntu):** don't use the distro `/usr/bin/pio`
(v4.3.4 crashes on Python 3.12). Install a current core in a venv:
`python3 -m venv ~/.pio-venv && ~/.pio-venv/bin/pip install -U platformio intelhex`,
and use `~/.pio-venv/bin/pio`. Serial-port access needs the `dialout` group (or
`sudo setfacl -m u:$USER:rw /dev/ttyUSB0` per session).

## Notes

- LA frequencies (62.5 Hz–1 kHz) are much slower than the serial (~kHz); at a
  timebase set for the serial the LA lines look near-DC, and vice-versa — that's
  expected. It's for verifying both paths capture at once, not for a single
  shared timebase.
- GPIO22/23 are not strapping pins. GPIO12 (LA D1) is a strapping pin but is only
  driven *after* boot, so normal operation is fine (keep nothing pulling it high
  at power-up).
- Decode the analog with `mso5202d_decode.py` (`--proto spi/uart/i2c`, channels
  `--clk 0 --data 1`); the LA path over USB remains limited (framebuffer only —
  see docs), so the LA here is mainly for on-scope viewing / future LA work.
- The command channel is the ESP32's own USB-serial console (UART0). The three
  test protocols are on CH1/CH2 (UART protocol uses UART1 on GPIO22), so runtime
  control never collides with the generated signals.
