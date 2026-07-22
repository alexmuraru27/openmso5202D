# MSO5202D — waveform rendering model

How samples from the scope become a trace on screen: the two sample sources, the
byte→volts decode each needs, and the axis conventions the viewer draws them on.
The wire format the samples arrive in is `MSO5202D-protocol.md`; the flows that
fetch them are `MSO5202D-statemachines.md`.

---

## 1. Two sample sources

The scope offers samples in two forms, and they need different treatment.

| | Screen block (`0x02`) | Exported CSV |
| --- | --- | --- |
| Length | 3840 samples (3200 with a menu open) | 4064 / 40064 / 400064 rows |
| Value | raw **signed int8 counts** | **volts**, scope-calibrated |
| Time | implicit — 200 samples/division | explicit `time` column |
| Channels | one per acquire, sequential | one per file |
| Needs the card | no | yes |

**The app plots the CSV path.** A capture arms a single sequence, has the scope
export the record to the front-panel USB drive, and reads the file back, so the
value column is already volts and no counts conversion is involved (§3).

The screen block still matters, and §2 is its decode. The driver uses it for two
things that only need sample *shape*, not calibration: proving a channel is
actually acquiring (an off channel returns an empty block), and probing the
finest pulse in the signal so the capture planner can pick a timebase.

## 2. Screen block: bytes → divisions

Each analog sample byte is a **two's-complement signed int8** giving the trace's
vertical position in **counts at 25 counts per division**, clamped to `[−127, +127]`.
The device pre-positions the trace (it already folds in `VERT-CHx-POS`), and the
byte **rises as the trace moves up**. The canonical decode is a sign-extend:

```
s     = byte − 256  if byte ≥ 128  else byte      # sign-extend to signed int8
y_div = (s − 16) / 25                              # divisions from centre, up = +
```

The `25` counts/div scale is hardware-verified. The `−16` removes a ≈0.64-division
baseline bias — with the channel centred, the zero-signal baseline sits at byte
`+16`, not `0x00` — and is the one un-nailed constant.

Two traps:

- **Decode signed, and never `128 − byte`** — the latter inverts the motion.
- **There is no 8-bit wrap.** A small signal oscillating around 0 alternates
  `0xFF` (−1) ↔ `0x00` (0); read unsigned, that zero-crossing looks like a fake
  rail-to-rail hash block. Decoded signed it is an ordinary small waveform, and
  the `±127` clamp makes overflow impossible.

To label the axis in volts rather than divisions: **volts-per-count = Vdiv/25**,
so `volts ≈ (s − 16)/25 × Vdiv`. The scale is exact; only the `+16` baseline
offset is unresolved, which is why this path is used for shape, not measurement.

### Rails and clipping

- `0x0A` / `0xF2` are signed **+10 / −14** — an ordinary on-screen signal
  straddling the zero line, not rails. Do not reject them.
- The **saturation rails** are `0x7F` (+127) and `0x81` (−127): a trace parked
  fully above or below the graticule reads a solid run of one of them.
- `0x80` never occurs. It is a framing-error tell, not a sample.
- A trace parked off-screen is detected by **position**, not value:
  `|VERT-CHx-POS| / 25 > 4` puts it outside the 8-division screen.

**[gap]** A block can come back split ~50/50 between `0x0A` and `0xF2` with
nothing in between, after a trace is dragged off-screen and back. This is not the
±127 clamp and is unexplained; treat such a block as invalid.

### Horizontal

```
x_div = sample_index / 200
```

**200 samples per division**, hardware-verified (`SAMPLES_PER_DIV` in
`backend/src/settings/mod.rs`). Multiply by the timebase (`TDIV-ns`) for seconds.

The block width is **not fixed** — read the count from the acquire size frame.
It is 3840 (19.2 div) normally and 3200 (16 div) while a soft-menu panel is open
on the scope, and it does not depend on the timebase.

## 3. Exported CSV: volts and real time

An export carries its own axes, so there is nothing to calibrate
(`backend/src/waveform.rs`, `parse_csv`):

- The **value column is volts** for an analog export, already scope-calibrated.
- The **time column is seconds**. The sample interval is taken as the **median
  step between timestamps**, not from the `#timebase` header — that header is the
  screen time/div (in picoseconds), and a deep record samples faster than the
  screen does, so deriving `dt` from it would be wrong.
- `#voltbase` is **µV/div**: the scope's own vertical scale, carried through as
  `voltsPerDiv` so the viewer can annotate a lane with the instrument's V/div.
- A logic-analyzer export replaces `#voltbase` with `#threshold` (millivolts) and
  its value column holds a **16-bit word per sample**, bit `N` being channel D`N`.

## 4. Axes in the viewer

The viewer draws **one lane per channel**, stacked, sharing a single time axis.

**Vertical.** Each lane is fitted **once per record** to that channel's own
min/max, then the user's zoom and pan are applied on top of that fitted range.
The fit is per record, not per frame: nothing re-scales underneath a trace while
it is being read, which is the property that makes a scope face readable. Because
CH1 and CH2 can be on different V/div, each lane keeps its own volt range and
prints its scale, pk-pk and probe factor in its corner rather than forcing a
shared axis.

**Horizontal.** Time is shown **signed about the trigger**, which sits at the
midpoint of the record — the acquisition centres it — so the axis reads
`−…, 0, +…` around the trigger event rather than from an arbitrary zero. Labels
use one unit for the whole axis, with just enough decimals to keep adjacent ticks
distinct at any zoom.

**Colours.** One per channel, matching the instrument: CH1 yellow, CH2 blue.

## 5. Logic-analyzer traces

The index→x mapping is shared; only the vertical layout differs. Each digital
channel is drawn as **its own horizontal row** — a 0/1 value scaled to a small
fixed row height, offset to that channel's baseline, labelled D0…D15, stacked
lowest-Dn-at-bottom.

The pod can only be rendered from a **saved CSV** (Source = LA in the export
menu). There is no live path: the raw LA acquire is a broken firmware route that
returns unreliable data *and* corrupts the scope's own display while reading, so
it is never issued. Enabling the pod also clamps store depth to 4K — deep memory
is analog-only. The other way to *see* the pod over USB is the `0x20`
framebuffer, which is the scope's own rendered screen including its D-rows.

## 6. Decoded-byte overlay

When a protocol decode is active, each decoded byte is drawn over the stretch of
waveform it was read from: a pill carrying the value, plus faint alternating
byte-boundary slices tying it to the samples underneath. Slices too narrow to
read at the current zoom are culled — the pills still carry those — and anything
off-screen is skipped, so the overlay stays legible at every magnification.

## 7. Known limits

- The **`+16`-count baseline** in §2 is the one unresolved constant in the
  counts→volts model. It does not affect the CSV path, which is pre-calibrated.
- **Inter-channel phase** holds only for a frozen acquisition. While the scope is
  running, CH1 and CH2 are separate sequential acquires and their relative timing
  is meaningless; a stop freezes one simultaneous two-channel acquisition, and
  both channels then read from that same buffer. Every capture the app takes is
  of the frozen kind, which is what makes two-channel protocol decode possible.
