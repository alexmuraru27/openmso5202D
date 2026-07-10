# esp_uart_gen — MSO5202D UART test-signal generator

A PlatformIO/Arduino sketch for an **ESP-WROOM-32** that streams the
`0x00..0xFF` ramp over an **8N1 UART** line, so the scope's UART decoder
(`scripts/mso5202d_decode.py`) can be checked byte-for-byte — any decode slip
shows up instantly as a break in the count.

Uses the ESP32 **hardware UART** (exact baud/framing — a bit-bang generator's
timing jitter was enough to corrupt decodes), LSB-first, with a ~3-bit idle gap
between bytes so the decoder re-syncs each frame.

## Wiring

| ESP32 | Scope | Role |
|-------|-------|------|
| GPIO13 | CH1 | UART TX (`--line 0`) |
| GND | probe ground | common ground (required) |

CH2 is unused for UART. Output is **3.3 V CMOS**. Set the scope channel to
**1× probe, DC coupling, invert Off**, ~1 V/div. Suggested timebase depends on
baud (≈25 samples/bit is plenty) — e.g. **800 µs/div at 9600**, **80 µs/div at
115200**; `mso5202d_decode.py capture --timebase <idx>` can set it for you.

## Build / flash

```bash
cd scripts/esp_uart_gen
~/.pio-venv/bin/pio run -t upload      # see esp_toggler/README.md for the pio venv setup
~/.pio-venv/bin/pio device monitor     # 115200 baud — prints the pin map
```

Baud is selectable at build time (default 9600):

```bash
PLATFORMIO_BUILD_FLAGS="-DBAUD=115200" ~/.pio-venv/bin/pio run -t upload
```

## Decode

```bash
python3 mso5202d_decode.py capture cap.npz --timebase 17
python3 mso5202d_decode.py decode  cap.npz --proto uart --line 0 --baud 9600
python3 mso5202d_decode.py view    cap.npz --proto uart --line 0            # waveform + overlay
```

`--baud` is optional (the decoder auto-detects from the shortest pulse); pass it
to lock the rate. **Verified 9600 – 115200 baud** on real hardware.

## Notes

- GPIO13 is not a strapping pin (safe at boot).
- Captures occasionally come back glitched (more so at slow timebases); just
  re-capture — the decoder is deterministic, the scope acquisition isn't always.
