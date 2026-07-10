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

- **CH1/CH2 — a serial protocol** (selectable), looping the `0x00..0xFF` ramp:
  - `PROTO=0` **SPI** (default): SCLK=GPIO22, MOSI=GPIO23, mode 0, MSB, ~20 kHz
  - `PROTO=1` **UART**: TX=GPIO22, 8N1 9600 baud (CH2 unused)
  - `PROTO=2` **I²C**: SCL=GPIO22, SDA=GPIO23, self-ACK ~50 kHz
- **LA D0…D15 — a distinct frequency per line**, `f_N = 1000/(N+1)` Hz
  (D0 = 1 kHz … D15 = 62.5 Hz) — a known, per-channel-identifiable LA pattern.

The LA runs continuously (non-blocking `micros()` scheduler); the serial is paced
between LA ticks, and SPI/UART use the hardware peripherals so their bit timing
stays exact regardless of the LA.

## Build / flash

```bash
cd scripts/esp_combo_gen
~/.pio-venv/bin/pio run -t upload            # SPI (default)
PLATFORMIO_BUILD_FLAGS="-DPROTO=1" ~/.pio-venv/bin/pio run -t upload   # UART
PLATFORMIO_BUILD_FLAGS="-DPROTO=2" ~/.pio-venv/bin/pio run -t upload   # I2C
~/.pio-venv/bin/pio device monitor           # 115200 baud — prints the pin map
```

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
