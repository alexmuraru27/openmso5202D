# MSO5202D — waveform rendering model

How to turn the raw waveform bytes from the scope into a trace that looks like
the instrument's own screen. This is the model `scripts/mso5202d_plot.py`
implements; the wire format the bytes arrive in is in `MSO5202D-protocol.md` §5,
and a condensed version of this model lives there under "Reference rendering
model."

The single most important rule: **use a fixed scale, never auto-fit to the
data.** A real scope's grid never moves; the trace grows and shrinks *within* a
fixed graticule. Auto-scaling each frame is what makes a naïve plotter look
"sloppy" — the baseline and amplitude jump around between reads.

---

## 1. The graticule

Draw a fixed **8 divisions tall × 10 divisions wide** grid (the scope face),
with **5 minor subdivisions per division** and a bold centre line, on a dark
background. Our acquired block is 3840 samples = **19.2 divisions** wide (see
§3), so the viewer simply extends the grid to the full captured width; the
vertical is always exactly 8 divisions (−4 … +4).

Everything below places the trace onto this grid in **division** units. Because
CH1 and CH2 can be on different V/div, *divisions* — not volts — are the honest
shared vertical axis, exactly as printed on the instrument's screen. Each
channel's volts/div is shown in the title.

## 2. Vertical mapping (bytes → divisions)

Each analog sample byte is a **two's-complement signed int8** giving the trace's
vertical position in **counts at 25 counts per division**, clamped to `[−127, +127]`.
The device pre-positions the trace (it already folds in `VERT-CHx-POS`), and the
byte **rises as the trace moves up**. The canonical decode is just a sign-extend:

```
s     = byte − 256  if byte ≥ 128  else byte      # sign-extend to signed int8
y_div = (s − 16) / 25                              # divisions from centre, up = +
```

The `25` counts/div scale is hardware-verified; the `−16` removes a ≈0.64-division
baseline bias (with the channel centred, the zero-signal baseline sits at byte
`+16`, not `0x00`) and is the one un-nailed constant.

Two traps to avoid:

- **Do not decode unsigned, and do not do `128 − byte`** (that inverts the motion).
- **There is no 8-bit "wrap".** A small signal oscillating around 0 alternates
  `0xFF` (−1) ↔ `0x00` (0); an *unsigned* reader misreads that zero-crossing as a
  fake rail-to-rail "hash" block. Decoded signed, it is an ordinary small waveform.
  The `±127` clamp makes overflow impossible.

**Equivalent position-unwrap form** — what `to_divs()` in the plotter uses; kept
for continuity, gives the identical result:

```
base   = (VERT-CHx-POS + 16) & 0xFF
signal = ((byte − base + 128) mod 256) − 128     # AC counts
y_div  = (VERT-CHx-POS + signal) / 25            # divisions, up = positive
```

Because `byte = POS + 16 + signal` (clamped, no surviving wrap), this collapses to
`y_div = (s − 16)/25`. See **MSO5202D-protocol.md §6** for the full derivation. A
trace parked off-screen saturates at the rails `0x7F` (+127) / `0x81` (−127), and
the axis (fixed at ±4 div) clips it away.

## 3. Horizontal mapping (index → divisions)

```
x_div = sample_index / 200
```

**200 samples per division** (hardware-verified; `SAMPLES_PER_DIV`). The time per
division is in the settings blob (`TDIV-ns`); multiply `x_div` by it for seconds.

The **block width is not fixed** — read the sample count from the acquire *size*
frame, don't assume 3840. It is **3840** (19.2 div) normally but **3200** (16 div)
when a soft-menu panel is open on the scope, and it does **not** depend on the
timebase (MSO5202D-protocol.md §6.3). The viewer extends the grid to whatever width
came back.

## 4. Rail values and off-screen blocks

The signed-int8 model (§2) settles what the near-"rail" bytes are:

- `0x0A` / `0xF2` are just signed **+10 / −14** — a normal on-screen signal
  straddling the zero line, **not** rails. Do not reject them. (Earlier versions
  wrongly treated ≈`0x08`/`0xF2` first as clipped rails, then as "wrapped signal";
  both were unsigned mis-readings.)
- The **real saturation rails** are `0x7F` (+127) and `0x81` (−127): a trace parked
  fully above/below the graticule reads a solid run of one of these. `0x80` never
  occurs, so it is a framing-error tell, not a sample.
- Off-screen is detected by **position**: `|VERT-CHx-POS| / 25 > 4` means the trace
  is parked off the 8-division screen (the viewer flags it in the title).

**[open] genuine off-screen bimodal block:** separately, a whole block can come
back split ~50/50 between `0x0A` and `0xF2` (nothing in between) after a trace is
dragged off-screen and back. This is **not** the ±127 clamp and is unexplained;
treat such a block as invalid/off-screen and don't plot it
(MSO5202D-protocol.md §6 GAP).

## 5. Drawing style

- **Vectors** (default): connect consecutive points with straight line segments
  (a polyline) — what `DISPLAY-MODE` = 0 shows on the scope.
- **Dots**: plot the points without connecting them — `DISPLAY-MODE` = 1.
- One colour per channel (CH1 yellow, CH2 blue, matching the instrument).

## 6. Analog vs. logic-analyzer traces

The coordinate mapping (§2–§4) is shared. The difference is layout:

- **Analog** — one polyline per enabled channel, mapped as above and centred on
  the graticule.
- **Logic analyzer (D0–D15)** — the *same* index→x mapping, but each digital
  channel is drawn in **its own horizontal row**: a 0/1 value scaled to a small
  fixed row height and offset to that channel's baseline, with the channel label
  (D0…D15) beside it. The row-rendering code exists (`draw_la` in
  `mso5202d_plot.py`: enabled channels from `LA-CHANNEL-STATE` stacked lowest-Dn-
  at-bottom, green, `y = row_center − amp/2 + bit·amp`), but it is **disabled by
  default** (`LA_READ_ENABLED = False`).
  - **Why disabled:** the only raw LA read (`02 01 05`) is a non-functional
    firmware path — it returns unreliable 2-state data and **corrupts the scope's
    own LA display** while reading (see MSO5202D-protocol.md §5). So there is no
    safe raw LA sample source to render.
  - **The right way to show LA** is the scope's **`0x20` framebuffer** — its
    rendered screen already contains the firmware-drawn D0–D15 rows (this is how
    the vendor's virtual panel does it). A framebuffer/screen-mirror view is the
    intended path for LA in the viewer; the vector row-renderer above is kept for
    if/when a safe raw LA readout is ever found.

## 7. What our viewer implements (`scripts/mso5202d_plot.py`)

- `to_divs(bytes, pos)` — the §2 unwrap: baseline `(POS+16)&0xFF`, unwrap the
  signal, `y_div = (POS + signal)/25` (fixes the reverse movement and the centre
  hash in one step).
- `x_divs(n)` — §3 mapping.
- `style_scope(ax, width_div)` — the fixed 8×N graticule (§1), dark theme,
  minor subdivisions, centre line.
- Fixed axes (`ylim = ±4 div`), colours per channel, a title with the real units
  (V/div, time/div, sample rate, trigger state/level/frequency) and an
  off-screen warning.

Run `python3 mso5202d_plot.py --png out.png` for a headless frame, or with no
arguments for the live view.

## 8. Known limits carried over from the protocol

- **counts→volts** — the *scale* is now known exactly: **volts-per-count = Vdiv/25**
  (the ADC's own 25 counts/div, confirmed against the scope's exported CSV; see
  protocol.md §7). The viewer still shows **divisions** (each = that channel's
  V/div, in the title) because the *absolute* offset — the `+16`-count baseline
  (§2) — is the one unresolved constant. To label volts: `volts ≈ (s − 16)/25 × Vdiv`.
- **Inter-channel phase** is not preserved (CH1/CH2 are sequential acquires), so
  cross-channel timing on the plot is not meaningful (protocol.md §5).
