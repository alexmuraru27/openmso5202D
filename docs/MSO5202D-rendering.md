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

Each sample byte encodes **the channel's position *and* the signal, wrapped to
8 bits**:

```
byte = (VERT-CHx-POS + 16 + signal) mod 256
```

where `VERT-CHx-POS` is the channel position (1/25-div units) and `signal` is the
AC waveform in counts (**25 counts per division**). Two traps for a naïve decoder
follow directly:

- the byte **rises as the trace moves up** (reversed vs. a plain "small byte =
  top" rule), and
- as the trace nears screen centre the baseline nears the byte edge (0/256), so
  the signal **wraps around it** → a rail-to-rail "hash" block (the real signal
  folded across the 8-bit boundary, *not* a bad frame).

So do **not** map the raw byte directly. **Unwrap** each sample around the
POS-derived baseline — this undoes both the reversal and the wrap and places the
trace at its true division:

```
base   = (VERT-CHx-POS + 16) & 0xFF
signal = ((byte − base + 128) mod 256) − 128     # AC counts, unwrapped
y_div  = (VERT-CHx-POS + signal) / 25            # divisions, up = positive
```

The trace then sits at `POS/25` divisions — moving **up** when you raise it, like
the scope — with a clean ≈1-div signal on top, correct at **every** position
including dead centre (where the raw bytes are a rail-to-rail hash). Off-screen
(parked past ±4 div) the block flat-lines near mid-code (~129) and the axis clips
it away.

## 3. Horizontal mapping (index → divisions)

```
x_div = sample_index / 200
```

**200 samples per division** (hardware-verified; `SAMPLES_PER_DIV`). A full
3840-sample block is 19.2 divisions. The time per division is in the settings
blob (`TDIV-ns`); multiply `x_div` by it for seconds.

## 4. Rail values are *not* gaps — they are wrapped signal

An earlier version of this doc treated near-rail bytes (≈`0x08`/`0xF2`) as
off-screen and broke the line there. That was wrong: those bytes are the **real
signal wrapped across the 8-bit boundary** near screen centre (§2). The **unwrap**
in §2 recovers them into a clean trace, so there is nothing to break — do *not*
NaN or reject rail-valued samples.

Genuinely off-screen (parked) blocks flat-line near mid-code; after the §2 map
they land outside ±4 div and the axis clips them. The viewer flags a channel whose
position parks it off the 8-division screen (`|POS| / 25 > 4`) in the title.

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
  (D0…D15) beside it and the selected/enabled row highlighted. (Reading the LA
  sample stream over USB is not yet implemented in the driver, so the viewer
  currently draws the analog channels only; this row model is recorded here for
  when it is.)

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

- **counts→volts** is not yet calibrated in absolute terms, so the vertical axis
  is in divisions, not volts (each division = that channel's V/div, shown in the
  title). The 25 counts/div *scale* is known; the absolute offset is not.
- **Inter-channel phase** is not preserved (CH1/CH2 are sequential acquires), so
  cross-channel timing on the plot is not meaningful (protocol.md §5).
