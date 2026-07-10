# esp_spi_gen — MSO5202D SPI test-signal generator

A PlatformIO/Arduino sketch for an **ESP-WROOM-32** that clocks the `0x00..0xFF`
ramp out of the ESP32 **hardware SPI** peripheral (SCLK + MOSI), so the scope's
SPI decoder can be verified byte-for-byte. MSB-first; clock mode selectable at
build time.

There is no chip-select line (the scope only has 2 analog channels), so the
sketch leaves a short idle-clock gap (~4 clock periods) between bytes and the
decoder re-frames on that gap — staying aligned even if a capture starts
mid-byte.

## Wiring

| ESP32 | Scope | Role |
|-------|-------|------|
| GPIO13 | CH1 | SCLK (`--clk 0`) |
| GPIO14 | CH2 | MOSI (`--data 1`) |
| GND | probe ground | common ground (required) |

Output is **3.3 V CMOS**. Set **both** channels to **1× probe, DC coupling,
invert Off**, ~1 V/div. Pick a timebase so a clock period spans ≥~5 samples
(e.g. 500 µs/div at 20 kHz; scale down as you raise the clock).

## Build / flash

```bash
cd scripts/esp_spi_gen
~/.pio-venv/bin/pio run -t upload      # see esp_toggler/README.md for the pio venv setup
```

Clock rate and mode are build flags (default 20 kHz, mode 0):

```bash
PLATFORMIO_BUILD_FLAGS="-DSPI_HZ=1000000 -DSPI_MODE=0" ~/.pio-venv/bin/pio run -t upload
```

`SPI_MODE` is 0..3 (CPOL/CPHA) — it must match the decoder's `--cpol`/`--cpha`.

## Decode

```bash
python3 mso5202d_decode.py capture cap.npz --timebase 15
python3 mso5202d_decode.py decode  cap.npz --proto spi --clk 0 --data 1 --cpol 0 --cpha 0
python3 mso5202d_decode.py view    cap.npz --proto spi --clk 0 --data 1
```

Add `--lsb` if you build the generator LSB-first (it is MSB-first by default).
**Verified 10 kHz – 2 MHz SCLK** on real hardware (mode 0).

## Notes

- Only MOSI is captured — full-duplex SPI (MISO too) needs a 3rd channel the
  scope can't provide over USB. Wire MISO to CH2 instead if you want to decode
  the return direction.
- GPIO13/14 are not strapping pins (safe at boot).
