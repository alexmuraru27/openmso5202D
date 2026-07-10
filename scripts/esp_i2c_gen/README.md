# esp_i2c_gen — MSO5202D I²C test-signal generator

A PlatformIO/Arduino sketch for an **ESP-WROOM-32** that bit-bangs short I²C
write transactions of the `0x00..0xFF` ramp (SCL + SDA), so the scope's I²C
decoder can be verified byte-for-byte. Each transaction is `START, addr+W,
8 ramp data bytes, STOP` — so a START appears often and lands in most captures.

**Synthetic generator:** there is no real slave, so the master drives the ACK
slot low itself (self-ACK) and both lines are push-pull (not the true open-drain
bus). That yields a clean, textbook START/addr/data/ACK/STOP waveform for decode
testing — it is *not* a bus to hang real devices on.

## Wiring

| ESP32 | Scope | Role |
|-------|-------|------|
| GPIO13 | CH1 | SCL (`--scl 0`) |
| GPIO14 | CH2 | SDA (`--sda 1`) |
| GND | probe ground | common ground (required) |

Output is **3.3 V CMOS**. Set **both** channels to **1× probe, DC coupling,
invert Off**, ~1 V/div. Suggested timebase ~200 µs/div (scale down as you raise
the clock).

## Build / flash

```bash
cd scripts/esp_i2c_gen
~/.pio-venv/bin/pio run -t upload      # see esp_toggler/README.md for the pio venv setup
```

Bit rate is a build flag via the per-edge dwell `Q_US` (default 5 → ~67 kHz SCL;
SCL ≈ 1/(3·Q_US) MHz):

```bash
PLATFORMIO_BUILD_FLAGS="-DQ_US=2" ~/.pio-venv/bin/pio run -t upload   # ~167 kHz
```

## Decode

```bash
python3 mso5202d_decode.py capture cap.npz --timebase 15
python3 mso5202d_decode.py decode  cap.npz --proto i2c --scl 0 --sda 1
python3 mso5202d_decode.py view    cap.npz --proto i2c --scl 0 --sda 1
```

The decode shows `START`, the address byte as `50WA` (addr 0x50, Write, ACKed),
each data byte as `NNa`, and `STOP`. **Verified ~17 – 167 kHz** on real hardware
(the bit-bang generator's ceiling — the decoder itself is edge-driven and has no
inherent speed limit; a faster I²C source would decode too, timebase permitting).

## Notes

- GPIO13/14 are not strapping pins (safe at boot).
- `delayMicroseconds` under-1 µs is unreliable, so the bit-bang tops out around
  167 kHz; that already covers standard-mode (100 kHz) I²C.
