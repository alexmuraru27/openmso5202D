# esp_toggler — MSO5202D logic-analyzer test-signal generator

A PlatformIO/Arduino sketch for an **ESP-WROOM-32** (classic ESP32-S DevKitC,
e.g. the 38-pin AliExpress clone) that drives all **16 LA channels** of the
Hantek MSO5202D with distinguishable signals. Use it to verify the LA channel
mapping and to have known inputs while reverse-engineering the LA menus.

## Wiring (LA channel → ESP32-WROOM GPIO)

| LA | GPIO | LA | GPIO | LA | GPIO | LA | GPIO |
|----|------|----|------|----|------|----|------|
| L00 | D13 | L04 | D26 | L08 | D15 | L12 | D17 |
| L01 | D12 | L05 | D25 | L09 | D2  | L13 | D5  |
| L02 | D14 | L06 | D33 | L10 | D4  | L14 | D18 |
| L03 | D27 | L07 | D32 | L11 | D16 | L15 | D19 |

Also connect **ESP32 GND ↔ LA pod GND** (common ground is required). Outputs are
**3.3 V CMOS** — set the scope's LA threshold to a normal TTL/CMOS level
(~1.4–1.6 V).

## Patterns

Selected at build time via `-DPATTERN` in `platformio.ini` (default = FREQ).

- **FREQ (default)** — each channel is an independent square wave,
  **f_N = 1000 / (N+1) Hz**: L00 = 1000 Hz (fastest) … L15 = 62.5 Hz (slowest).
  The 16:1 span means all 16 are visible on one screen; identify any channel by
  measuring its frequency and computing `N = round(1000/f) − 1`. Adjacent
  channels differ by ~10 %, so read periods rather than eyeballing.
- **COUNTER** (`build_flags = -DPATTERN=1`) — a free-running 16-bit binary
  counter, **channel N = counter bit N** (L00 toggles fastest, L15 slowest,
  each ½ the previous). You can literally read the count and instantly spot a
  swapped or dead line — but the frequency span is huge (L00 = 5 kHz,
  L15 ≈ 0.08 Hz), so you can't see all bits on one timebase. Best for a
  low-bits ordering check; multi-bit transitions are glitch-free (simultaneous
  register write).

## Build / flash / monitor

```bash
cd scripts/esp_toggler
pio run -t upload          # compile + flash (auto-detects the port)
pio device monitor         # 115200 baud — prints the pin→freq map at boot
```

Verified building clean (both patterns): RAM 6.6 %, Flash 20.6 %.

**PlatformIO setup gotchas (seen on this Debian/Ubuntu box):**

- **Don't use the distro `python3-platformio` / `/usr/bin/pio`** — it's v4.3.4 and
  crashes on Python 3.12 (`AttributeError: 'PlatformioCLI' ... resultcallback`).
  Install a current core instead, isolated from system Python:
  ```bash
  python3 -m venv ~/.pio-venv && ~/.pio-venv/bin/pip install -U platformio
  ~/.pio-venv/bin/pio run -t upload        # use this pio
  ```
  (Or just use the **VS Code PlatformIO extension**, which bundles its own core.)
- If the build fails at `bootloader.bin` with
  `ModuleNotFoundError: No module named 'intelhex'`, install it into the same
  env: `~/.pio-venv/bin/pip install intelhex`, then rebuild.

## Using it with the scope

1. Flash, open the serial monitor, confirm the printed L00…L15 → GPIO/freq map.
2. On the scope: enable the LA, turn on D0…D15, set threshold ~1.5 V, pick a
   timebase (~2 ms/div shows ~1.5 cycles of L15 and many of L00 in FREQ mode).
3. Each LA channel should show its expected rate; a channel that's flat or at
   the wrong frequency reveals a wiring/decode mismatch. This is the known-good
   input for mapping the LA menu (`LA-CHANNEL-STATE`, thresholds — protocol
   doc §6 / Appendix D).

## Notes / caveats

- **Classic ESP32 only.** The COUNTER mode uses the classic ESP32 GPIO register
  layout (`GPIO.out1_w1ts`). It won't compile unchanged for ESP32-S2/S3/C3.
- **Strapping pins.** Several LA lines land on ESP32 strapping pins (GPIO12, 2,
  4, 5, 15). They're only driven *after* boot, so normal operation is fine, but
  if the board ever fails to boot, make sure nothing pulls **GPIO12 (L01) high
  at power-up** (a high MTDI selects 1.8 V flash → brownout). LA probes are
  high-impedance, so this is normally a non-issue.
- All 16 pins are output-capable; none are input-only (GPIO34–39 are avoided),
  and none touch the flash pins (GPIO6–11).
